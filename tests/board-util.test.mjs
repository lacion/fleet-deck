// tests/board-util.test.mjs
//
// Pure tests for the board's model-display helpers (board/src/util.js).
//
// Getting the model ID right in the daemon is only half the fix: the board then
// has to render it. The old prettyModel split on '-' and never rejoined the
// version, so a correctly-reported 'claude-opus-4-8' still rendered as the
// nonsense "Opus 4 8" — in a grey badge, because modelFamily only knew the
// fable/quill/comet families. This file pins both.
//
// board/src/util.js has no imports and board/package.json is "type": "module",
// so it loads under node --test with no bundler.

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  MODEL_FAMILIES,
  TERMWIN_EDGE,
  TERMWIN_MIN,
  clampWinRect,
  copyText,
  imageFromClipboard,
  isTermCopyChord,
  isTermPasteChord,
  pasteTextSafe,
  unwrapTmuxPassthrough,
  termChordHints,
  batchTotal,
  expandBatchTasks,
  modelFamily,
  modelShort,
  parseBatchTasks,
  prettyModel,
  sessionTicker,
} from '../board/src/util.js';
import { HOTKEYS, ORCH_COMMANDS } from '../board/src/helpText.js';

const HERE = path.dirname(fileURLToPath(import.meta.url));

// The full badge, as a human reads it on a card.
const PRETTY = [
  ['claude-fable-5', 'Fable 5'],
  ['claude-opus-4-8', 'Opus 4.8'],           // was "Opus 4 8"
  ['claude-opus-4-8[1m]', 'Opus 4.8 1M'],    // the long-context variant earns its pixels
  ['claude-sonnet-4-5-20250929', 'Sonnet 4.5'],
  ['claude-haiku-4-5-20251001', 'Haiku 4.5'],
  ['claude-3-5-haiku-20241022', 'Haiku 3.5'], // legacy version-first id, hoisted
  ['claude-sonnet-5', 'Sonnet 5'],
  ['claude-fable-5-mini', 'Fable 5 Mini'],    // qualifier keeps its position
  ['', '—'],
  [null, '—'],
  [undefined, '—'],
];

for (const [id, want] of PRETTY) {
  test(`prettyModel(${JSON.stringify(id)}) → ${want}`, () => {
    assert.equal(prettyModel(id), want);
  });
}

test('prettyModel is idempotent — a payload may already carry a pretty display_name', () => {
  for (const [, want] of PRETTY) assert.equal(prettyModel(want), want);
});

test('modelShort keeps the version but drops the 1M marker — compact cards have no room', () => {
  assert.equal(modelShort('claude-fable-5'), 'F5');
  assert.equal(modelShort('claude-opus-4-8'), 'O4.8');
  assert.equal(modelShort('claude-opus-4-8[1m]'), 'O4.8');
  assert.equal(modelShort('claude-sonnet-4-5-20250929'), 'S4.5');
  assert.equal(modelShort('claude-3-5-haiku-20241022'), 'H3.5');
  assert.equal(modelShort('claude-fable-5-mini'), 'F5M');
  assert.equal(modelShort(null), '—');
});

test('modelFamily names every family, case-insensitively, and falls back to other', () => {
  assert.equal(modelFamily('claude-opus-4-8'), 'opus');
  assert.equal(modelFamily('claude-opus-4-8[1m]'), 'opus');
  assert.equal(modelFamily('claude-sonnet-4-5-20250929'), 'sonnet');
  assert.equal(modelFamily('claude-3-5-haiku-20241022'), 'haiku');
  assert.equal(modelFamily('claude-fable-5'), 'fable');
  assert.equal(modelFamily('CLAUDE-OPUS-4-8'), 'opus');
  assert.equal(modelFamily('some-unknown-model'), 'other');
  assert.equal(modelFamily(null), 'other');
});

// ------------------------------------------------------------------ batch spawn

test('batch: one agent per line, blank lines ignored', () => {
  const tasks = parseBatchTasks('  fix the flaky test  \n\n update the README \n');
  assert.deepEqual(tasks, [
    { count: 1, prompt: 'fix the flaky test' },
    { count: 1, prompt: 'update the README' },
  ]);
  assert.equal(batchTotal(tasks), 2);
});

test('batch: an "Nx" prefix repeats that line, and expansion preserves order', () => {
  const tasks = parseBatchTasks('3x race a fix for the cap\nupdate the README\n2x audit spawn');
  assert.deepEqual(tasks, [
    { count: 3, prompt: 'race a fix for the cap' },
    { count: 1, prompt: 'update the README' },
    { count: 2, prompt: 'audit spawn' },
  ]);
  assert.equal(batchTotal(tasks), 6);
  assert.deepEqual(expandBatchTasks(tasks), [
    'race a fix for the cap', 'race a fix for the cap', 'race a fix for the cap',
    'update the README',
    'audit spawn', 'audit spawn',
  ]);
});

test('batch: the multiplier tolerates spacing, ×, and case', () => {
  assert.deepEqual(parseBatchTasks('3X  do it'), [{ count: 3, prompt: 'do it' }]);
  assert.deepEqual(parseBatchTasks('3 x do it'), [{ count: 3, prompt: 'do it' }]);
  assert.deepEqual(parseBatchTasks('3× do it'), [{ count: 3, prompt: 'do it' }]);
});

test('batch: a runaway multiplier is unrepresentable — the daemon has no cap left to save you', () => {
  // Two digits max. "300x ..." is not a 300-agent launch; it is a task whose
  // text begins "300x", which the preview will show you verbatim.
  assert.deepEqual(parseBatchTasks('300x spawn the world'), [{ count: 1, prompt: '300x spawn the world' }]);
  assert.equal(batchTotal(parseBatchTasks('99x go')), 99);
});

test('batch: text that merely looks like a multiplier is left alone', () => {
  // No trailing task after the prefix → it is just a prompt.
  assert.deepEqual(parseBatchTasks('2x'), [{ count: 1, prompt: '2x' }]);
  assert.deepEqual(parseBatchTasks('fix the 3x zoom bug'), [{ count: 1, prompt: 'fix the 3x zoom bug' }]);
});

test('batch: empty input launches nothing', () => {
  for (const empty of ['', '   ', '\n\n', null, undefined]) {
    assert.deepEqual(parseBatchTasks(empty), []);
    assert.equal(batchTotal(parseBatchTasks(empty)), 0);
  }
});

// -------------------------------------------------------------- sessionTicker

// The drawer filters the global ticker to the rows that name a callsign. Jira
// ticket suffixes prefix-nest ('raven-PROJ-1' ⊂ 'raven-PROJ-12'), where the old
// fixed-length hex suffix never could — so the match must be on callsign
// boundaries, not a bare substring, or the shorter ticket's timeline would leak
// into every longer one.
const tick = (...msgs) => msgs.map((msg, i) => ({ at: 1000 + i, msg }));

test('sessionTicker: a nested-prefix ticket does not steal the longer ticket\'s rows', () => {
  const t = tick(
    'raven-PROJ-12 edited util.js',   // the LONGER ticket — must not leak into PROJ-1
    'raven-PROJ-1 joined the fleet',  // PROJ-1's own row
  );
  const one = sessionTicker(t, 'raven-PROJ-1');
  assert.equal(one.length, 1);
  assert.equal(one[0].msg, 'raven-PROJ-1 joined the fleet');

  const twelve = sessionTicker(t, 'raven-PROJ-12');
  assert.equal(twelve.length, 1);
  assert.equal(twelve[0].msg, 'raven-PROJ-12 edited util.js');
});

test('sessionTicker: matches its own callsign at start, middle, and end of a message', () => {
  const t = tick(
    'raven-PROJ-1 joined the fleet',              // start
    '✉ mail for raven-PROJ-1 queued',             // middle
    'assigned to raven-PROJ-1',                    // end
    'unrelated otter-9c1a note',                   // no match
  );
  const rows = sessionTicker(t, 'raven-PROJ-1');
  assert.equal(rows.length, 3);
  assert.ok(rows.every((r) => r.msg.includes('raven-PROJ-1')));
});

test('sessionTicker: a rename line naming both callsigns is caught by both filters', () => {
  // The daemon ticks one handoff line containing the old and new names so both
  // cards' timelines catch the rename — do not depend on exact wording beyond
  // "contains both callsigns".
  const t = tick('renamed raven-4b7f → raven-PROJ-123 (ticket PROJ-123)');
  assert.equal(sessionTicker(t, 'raven-4b7f').length, 1);
  assert.equal(sessionTicker(t, 'raven-PROJ-123').length, 1);
});

test('sessionTicker: hex callsigns still match as before', () => {
  const t = tick(
    'falcon-a3f2 joined the fleet',
    '⚠ falcon-a3f2 and otter-91c4 both touched util.js',
    'otter-91c4 verifying',
  );
  const falcon = sessionTicker(t, 'falcon-a3f2');
  assert.equal(falcon.length, 2);
  assert.ok(falcon.every((r) => r.msg.includes('falcon-a3f2')));
  assert.equal(sessionTicker(t, 'otter-91c4').length, 2);
});

test('sessionTicker: tolerates empty/absent input and caps at 12 newest', () => {
  assert.deepEqual(sessionTicker(null, 'raven-PROJ-1'), []);
  assert.deepEqual(sessionTicker([], 'raven-PROJ-1'), []);
  const many = tick(...Array.from({ length: 20 }, () => 'raven-PROJ-1 tick'));
  assert.equal(sessionTicker(many, 'raven-PROJ-1').length, 12);
});

// ------------------------------------------------------------------ CSS guard

// The bug this fix half was: a family with no CSS renders a grey badge that
// looks like a bug in the daemon. Mechanize it so the next family added can't
// repeat it.
test('every family modelFamily() can return has a badge rule and tokens in BOTH themes', () => {
  const appCss = readFileSync(path.join(HERE, '..', 'board', 'src', 'app.css'), 'utf8');
  const tokens = readFileSync(path.join(HERE, '..', 'board', 'src', 'tokens.css'), 'utf8');
  // tokens.css is :root (dark) followed by [data-theme="light"] — split on the
  // light selector so each theme is checked for the pair independently.
  const cut = tokens.indexOf('[data-theme="light"]');
  assert.ok(cut > 0, 'tokens.css should define a light theme block');
  const themes = { dark: tokens.slice(0, cut), light: tokens.slice(cut) };

  for (const fam of MODEL_FAMILIES) {
    assert.ok(appCss.includes(`.fd-mbadge.${fam}`), `app.css is missing .fd-mbadge.${fam}`);
    for (const [name, block] of Object.entries(themes)) {
      assert.ok(block.includes(`--m-${fam}:`), `tokens.css ${name} theme is missing --m-${fam}`);
      assert.ok(block.includes(`--m-${fam}-bg:`), `tokens.css ${name} theme is missing --m-${fam}-bg`);
    }
  }
});

// --- image paste: clipboard-item selection (TermPane's paste handler shim) ---
//
// The DOM handler in TermPane must stay a thin shim, so the decision "does this
// clipboard carry an image we ingest?" lives here where node --test can reach
// it. The contract worth pinning: only file-kind image/* items count, the first
// one wins, and a text-only clipboard yields null — which is what lets ordinary
// text paste fall through to xterm untouched.
test('imageFromClipboard picks the first image file item', () => {
  const text = { kind: 'string', type: 'text/plain' };
  const png = { kind: 'file', type: 'image/png' };
  const jpeg = { kind: 'file', type: 'image/jpeg' };
  assert.equal(imageFromClipboard([text, png, jpeg]), png);
  assert.equal(imageFromClipboard([png]), png);
});

test('imageFromClipboard yields null when there is nothing to ingest', () => {
  assert.equal(imageFromClipboard(null), null);
  assert.equal(imageFromClipboard([]), null);
  assert.equal(imageFromClipboard([{ kind: 'string', type: 'text/plain' }]), null);
  // an image STRING item (e.g. an <img> URL) is not a pasted file
  assert.equal(imageFromClipboard([{ kind: 'string', type: 'image/png' }]), null);
  // a non-image file (e.g. a PDF) is not ours either
  assert.equal(imageFromClipboard([{ kind: 'file', type: 'application/pdf' }]), null);
});

// --- v2.6 floating terminal: rect clamping (TermWindow's geometry contract) ---
//
// The drag/resize math lives in clampWinRect so node --test can pin the one
// property that makes a floating window livable: the drag bar can NEVER leave
// the screen, whatever garbage localStorage or a monitor swap hands us.
test('clampWinRect keeps a sane rect unchanged', () => {
  const vp = { w: 1920, h: 1080 };
  const r = clampWinRect({ x: 100, y: 80, w: 900, h: 600 }, vp);
  assert.deepEqual(r, { x: 100, y: 80, w: 900, h: 600 });
});

test('clampWinRect pulls an off-screen rect back to a grabbable position', () => {
  const vp = { w: 1280, h: 800 };
  // shoved off the right/bottom: at least TERMWIN_EDGE must remain visible
  const r = clampWinRect({ x: 5000, y: 5000, w: 600, h: 400 }, vp);
  assert.equal(r.x, vp.w - TERMWIN_EDGE);
  assert.equal(r.y, vp.h - TERMWIN_EDGE);
  // shoved off the left: the window may hang out, but 48px stays reachable
  const l = clampWinRect({ x: -5000, y: 100, w: 600, h: 400 }, vp);
  assert.equal(l.x, TERMWIN_EDGE - 600);
  // the TOP edge hard-stops at 0 — the drag bar itself must never go above
  const t = clampWinRect({ x: 100, y: -50, w: 600, h: 400 }, vp);
  assert.equal(t.y, 0);
});

test('clampWinRect enforces min size and viewport fit', () => {
  const vp = { w: 1280, h: 800 };
  const small = clampWinRect({ x: 0, y: 0, w: 10, h: 10 }, vp);
  assert.equal(small.w, TERMWIN_MIN.w);
  assert.equal(small.h, TERMWIN_MIN.h);
  const big = clampWinRect({ x: 0, y: 0, w: 9999, h: 9999 }, vp);
  assert.equal(big.w, vp.w);
  assert.equal(big.h, vp.h);
});

test('clampWinRect sanitizes garbage to a centered default', () => {
  const vp = { w: 1600, h: 1000 };
  const r = clampWinRect({ x: NaN, y: 'nope', w: null, h: undefined }, vp);
  assert.equal(r.w, 1060);
  assert.equal(r.h, 720);
  assert.equal(r.x, Math.round((vp.w - 1060) / 2));
  assert.equal(r.y, Math.round((vp.h - 720) / 2));
  // an empty object (first run — nothing saved) is the same story
  const fresh = clampWinRect({}, vp);
  assert.equal(fresh.w, 1060);
  assert.ok(fresh.x >= TERMWIN_EDGE - fresh.w && fresh.x <= vp.w - TERMWIN_EDGE);
});

// --- v2.6 help overlay: the hotkey list and the handler cannot drift ---------
//
// helpText.js is what the human READS ("?" overlay); useBoardHotkeys.js is what
// the board DOES. Pin that every key named in HOTKEYS appears in the handler
// source, so removing/renaming a binding without updating the help (or vice
// versa) fails here instead of silently lying to the user.
test('every documented hotkey exists in the useBoardHotkeys source', () => {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'hooks', 'useBoardHotkeys.js'), 'utf8');
  const expectations = [
    ["'j'", 'j'], ["'k'", 'k'], ["'ArrowDown'", '↓'], ["'ArrowUp'", '↑'],
    ["'y'", 'y'], ["'n'", 'n'], ['[1-9]', '1-9'], ["'Enter'", 'Enter'],
    ["'c'", 'c'], ["'?'", '?'], ["'Escape'", 'Esc'],
  ];
  for (const [needle, label] of expectations) {
    assert.ok(src.includes(needle), `useBoardHotkeys.js lost the ${label} binding that helpText.js documents`);
    assert.ok(
      HOTKEYS.some((h) => h.keys.includes(label)),
      `helpText.js HOTKEYS is missing the ${label} binding`,
    );
  }
});

test('every Compose chip inserts a prefix its own command grammar starts with', () => {
  for (const c of ORCH_COMMANDS.filter((x) => x.chip)) {
    assert.ok(
      c.syntax.startsWith(c.chip.trimEnd()),
      `chip "${c.chip}" does not match its syntax "${c.syntax}"`,
    );
  }
});

// --- the git-output disclosure on a failed spawn (source-grep guards) --------
//
// There is no JSX runner in this repo, so the two regressions that would silently
// destroy this feature are pinned by reading the sources. Both are invisible in
// review and obvious only on a real failed clone, which is exactly why they are
// mechanized here.
test('the failed-spawn git output is read by BOTH the card and the drawer', () => {
  const card = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'SessionCard.jsx'), 'utf8');
  const drawer = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'Drawer.jsx'), 'utf8');
  assert.ok(card.includes('fail_detail'), 'SessionCard.jsx no longer reads spawn.fail_detail');
  assert.ok(drawer.includes('fail_detail'), 'Drawer.jsx no longer reads spawn.fail_detail');
  // keyboard reachability: a real <button> carrying disclosure state, not a
  // div with an onClick (which Tab never reaches and Enter never fires)
  assert.match(card, /className="fd-failtoggle"/, 'the card lost the fd-failtoggle disclosure button');
  assert.match(card, /aria-expanded=\{failOpen\}/, 'the disclosure button lost aria-expanded');
});

test('the git-output block rides above the card overlay and cannot widen a lane', () => {
  const appCss = readFileSync(path.join(HERE, '..', 'board', 'src', 'app.css'), 'utf8');
  // Regression 1: .fd-cardopen is a full-bleed z-index:1 overlay. If the block is
  // not raised above it, the toggle opens the drawer instead and the revealed
  // text is unselectable — i.e. the remedy URL cannot be copied, which is the
  // whole feature. Assert both selectors sit in the raise rule.
  const raise = appCss.slice(appCss.indexOf('.fd-card .fd-cardacts .fd-actbtn'));
  const rule = raise.slice(0, raise.indexOf('}') + 1);
  assert.ok(rule.includes('z-index: 2'), 'the card raise rule moved — re-check this guard');
  assert.ok(rule.includes('.fd-card .fd-faildiag'), 'app.css stopped raising .fd-faildiag above .fd-cardopen');
  assert.ok(rule.includes('.fd-card .fd-faildiag pre'), 'app.css stopped raising the revealed <pre> above .fd-cardopen');
  // Regression 2: the lane grid is repeat(5, minmax(150px, 1fr)); an unwrapped or
  // unbounded <pre> gives the whole board horizontal scroll and buries the cards
  // below it.
  const pre = appCss.slice(appCss.indexOf('.fd-faildiag pre {'));
  const preRule = pre.slice(0, pre.indexOf('}') + 1);
  assert.ok(preRule.includes('white-space: pre-wrap'), '.fd-faildiag pre lost white-space: pre-wrap');
  assert.ok(preRule.includes('overflow-wrap: anywhere'), '.fd-faildiag pre lost overflow-wrap: anywhere');
  assert.match(preRule, /max-height: \d+px/, '.fd-faildiag pre lost its max-height');
  assert.ok(preRule.includes('overflow: auto'), '.fd-faildiag pre lost overflow: auto');
  // Regression 3 (contrast): a failed clone is tombstoned by construction, so this
  // block always renders inside `.fd-card.offline { opacity: .45 }`. Opacity is a
  // group property — nothing inside the block can undo it — which puts the revealed
  // <pre> under AA on the one card whose purpose is reading and copying a key. The
  // un-dim is therefore load-bearing, not polish, and the revive-chip exemption
  // never covers it (a failed clone has no worktree, so it is not `revivable`).
  assert.match(appCss, /\.fd-card\.offline:has\(\.fd-failtoggle\[aria-expanded="true"\]\) \{ opacity: \.\d+; \}/,
    'app.css stopped un-dimming the tombstoned card while the git output is open');
});

test('the drawer names the remote as the author of the git output', () => {
  // Not cosmetic: `remote:` lines are written by whoever runs the far end of the
  // clone, the drawer puts a copy button next to them, and the UI copy trains the
  // operator to lift a URL or a key straight out. The provenance caveat is the only
  // thing standing between that and a phishing surface, so it is pinned.
  const drawer = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'Drawer.jsx'), 'utf8');
  const appCss = readFileSync(path.join(HERE, '..', 'board', 'src', 'app.css'), 'utf8');
  assert.match(drawer, /className="src">relayed from the remote server</,
    'Drawer.jsx dropped the "relayed from the remote server" provenance label');
  assert.ok(appCss.includes('.fd-faildiag .hd .src'), 'app.css lost the styling for that label');
});

test('the SHIPPED board-dist actually contains the git-output feature', () => {
  // CI's board gate is `npm run build:board && git diff --exit-code
  // scripts/fleetd/board-dist`, and `git diff` CANNOT SEE UNTRACKED FILES. A
  // commit made with `git commit -a` stages the DELETED old hashed chunks and the
  // rewritten index.html but silently drops the NEW ones, and CI still exits 0 —
  // over an index.html referencing bundles that are not in the repo. Marketplace
  // installs track git main ungated, so main would serve a blank board.
  //
  // These two assertions convert that class of accident from "green CI, blank
  // board" into a test failure: every asset index.html references must exist, and
  // the referenced bundles must carry this change.
  const distDir = path.join(HERE, '..', 'scripts', 'fleetd', 'board-dist');
  const html = readFileSync(path.join(distDir, 'index.html'), 'utf8');
  const refs = [...html.matchAll(/(?:src|href)="\.\/(assets\/[^"]+)"/g)].map(m => m[1]);
  assert.ok(refs.length >= 2, `index.html should reference the built assets; found ${refs.length}`);
  for (const ref of refs) {
    assert.ok(existsSync(path.join(distDir, ref)),
      `board-dist/index.html references ${ref}, which is not on disk — the rebuilt asset was never staged`);
  }
  const js = refs.filter(ref => ref.endsWith('.js')).map(ref => readFileSync(path.join(distDir, ref), 'utf8')).join('\n');
  const css = refs.filter(ref => ref.endsWith('.css')).map(ref => readFileSync(path.join(distDir, ref), 'utf8')).join('\n');
  assert.ok(js.includes('fail_detail'), 'the shipped board bundle predates spawn.fail_detail — rerun npm run build:board');
  assert.ok(css.includes('fd-faildiag'), 'the shipped board stylesheet predates .fd-faildiag — rerun npm run build:board');
});

// --- copying out of a live terminal pane ------------------------------------
//
// Reported 2026-07-25: "I open a terminal and copy from the deck, it's not
// really copying anything, I can't paste." Two independent causes, both of them
// invisible from the board:
//
//   1. Claude Code's TUI turns mouse reporting ON (tmux: mouse_any_flag=1), so
//      xterm forwards a plain drag to the AGENT and makes no selection at all.
//      There was nothing to copy — the drag only looked like a selection.
//   2. Even with a selection, Ctrl+C never reached the clipboard: xterm sends
//      it as ETX (the interrupt) and cancels the keydown, so the browser's own
//      `copy` event never fires. So "copy" silently interrupted the agent.
//
// The fix claims Ctrl+C (⌘C on a Mac) ONLY when something is selected, and
// clears the selection afterwards so the next press interrupts as always. These
// pin the decision table — the DOM shim in TermPane is a two-line call.

const KEYDOWN = { type: 'keydown', key: 'c' };

test('Ctrl+C is the copy chord off a Mac, and ⌘C on one', () => {
  assert.equal(isTermCopyChord({ ...KEYDOWN, ctrlKey: true }, false), true);
  assert.equal(isTermCopyChord({ ...KEYDOWN, metaKey: true }, true), true);
  // Each platform's OTHER chord stays the agent's: ⌘C means nothing to a TUI,
  // and on a Mac Ctrl+C is still the interrupt.
  assert.equal(isTermCopyChord({ ...KEYDOWN, metaKey: true }, false), false);
  assert.equal(isTermCopyChord({ ...KEYDOWN, ctrlKey: true }, true), false);
});

test('the copy chord never swallows a key the agent needs', () => {
  const no = (e, why) => assert.equal(isTermCopyChord(e, false), false, why);
  no({ ...KEYDOWN }, 'bare c must type a c');
  no({ ...KEYDOWN, ctrlKey: true, shiftKey: true }, 'Ctrl+Shift+C belongs to devtools');
  no({ ...KEYDOWN, ctrlKey: true, altKey: true }, 'Ctrl+Alt+C is not the copy chord');
  no({ ...KEYDOWN, key: 'v', ctrlKey: true }, 'Ctrl+V is the paste path');
  no({ ...KEYDOWN, key: 'd', ctrlKey: true }, 'Ctrl+D must still reach the agent');
  no({ type: 'keyup', key: 'c', ctrlKey: true }, 'keyup would copy a second time');
  no({ type: 'keypress', key: 'c', ctrlKey: true }, 'keypress would copy a second time');
  no(null, 'a missing event is not a chord');
  // Caps Lock still spells the chord: xterm reports key:'C' with no shiftKey.
  assert.equal(isTermCopyChord({ type: 'keydown', key: 'C', ctrlKey: true }, false), true);
});

test('the hint bar names the chord the platform actually answers to', () => {
  assert.deepEqual(termChordHints(false), { select: '⇧drag', copy: 'Ctrl+C' });
  assert.deepEqual(termChordHints(true), { select: '⌥drag', copy: '⌘C' });
});

test('TermPane wires the copy chord AND the Mac escape hatch for selecting', () => {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  assert.match(src, /isTermCopyChord\(e[^)]*\)\s*&&\s*copySelection\(\)/,
    'TermPane no longer claims the copy chord — Ctrl+C is back to interrupting the agent');
  assert.ok(src.includes('term.clearSelection()'),
    'TermPane must clear the selection after copying, or Ctrl+C stops interrupting');
  // Off a Mac xterm forces a local selection on Shift for free; on a Mac the
  // escape hatch is ⌥ and it is OFF unless this option is set, which would
  // leave a Mac with no way to select pane text at all.
  assert.ok(src.includes('macOptionClickForcesSelection: true'),
    'TermPane dropped macOptionClickForcesSelection — a Mac cannot select pane text without it');
  // copyText, not navigator.clipboard: the LAN board is plain http, where
  // navigator.clipboard does not exist.
  assert.ok(/import \{[^}]*copyText[^}]*\} from '\.\.\/util\.js'/.test(src),
    'TermPane must copy through util.js copyText — navigator.clipboard is absent on the LAN board');
});

test('both terminal frames tell the human how to select and copy', () => {
  for (const file of ['TermWindow.jsx', 'TermGrid.jsx']) {
    const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', file), 'utf8');
    assert.ok(src.includes('termChordHints'), `${file} lost the select/copy hint`);
    assert.match(src, /CHORDS\.select.*CHORDS\.copy/s, `${file} names only half the chord pair`);
  }
});

test('the SHIPPED board-dist actually contains the copy chord', () => {
  // Same hazard the git-output test above pins — a green board gate over a
  // board-dist that predates the fix — but the terminal lives in a LAZY chunk
  // index.html never names, so that test's entry-only scan cannot see it. Walk
  // the import graph instead: every chunk the entry reaches must be on disk (the
  // untracked-new-hash accident), and the fix must be somewhere in it.
  const assetsDir = path.join(HERE, '..', 'scripts', 'fleetd', 'board-dist', 'assets');
  const html = readFileSync(path.join(assetsDir, '..', 'index.html'), 'utf8');
  const queue = [...html.matchAll(/(?:src|href)="\.\/assets\/([^"]+\.js)"/g)].map(m => m[1]);
  assert.ok(queue.length, 'index.html references no JS entry at all');
  const seen = new Set();
  const sources = [];
  while (queue.length) {
    const name = queue.shift();
    if (seen.has(name)) continue;
    seen.add(name);
    const file = path.join(assetsDir, name);
    assert.ok(existsSync(file),
      `board-dist references assets/${name}, which is not on disk — the rebuilt chunk was never staged`);
    const src = readFileSync(file, 'utf8');
    sources.push(src);
    // Vite emits lazy chunks as bare relative specifiers inside the parent
    // chunk: import("./TermPane-<hash>.js").
    for (const m of src.matchAll(/["'`]\.\/([\w.-]+\.js)["'`]/g)) queue.push(m[1]);
  }
  assert.ok(seen.size > 1, 'the lazy terminal chunks dropped out of the graph walk');
  const js = sources.join('\n');
  // Both markers survive minification: an options-object property name and a
  // string literal.
  assert.ok(js.includes('macOptionClickForcesSelection'),
    'the shipped board bundle predates the pane copy fix — rerun npm run build:board');
  assert.ok(js.includes('selection cleared'),
    'the shipped board bundle predates the pane copy fix — rerun npm run build:board');
});

test('the SHIPPED board-dist asset graph covers every referenced non-JS asset too', () => {
  // The graph walk above follows only .js specifiers, but Vite also hands the
  // runtime a lazy CSS dependency map (__vite__mapDeps) and index.html links
  // the entry stylesheet. A lazy stylesheet that is rebuilt but never staged
  // passes both that walk and a `git diff` gate (the rebuild is UNTRACKED),
  // and the board then ships a terminal whose lazy import 404s. Walk every
  // referenced asset — JS, CSS, font, image — and reject stale extras on disk
  // that nothing references.
  const assetsDir = path.join(HERE, '..', 'scripts', 'fleetd', 'board-dist', 'assets');
  const html = readFileSync(path.join(assetsDir, '..', 'index.html'), 'utf8');
  const ASSET = /\.(?:js|css|woff2?|ttf|otf|png|jpe?g|gif|svg|webp|ico)$/;
  const queue = [...html.matchAll(/(?:src|href)="\.\/assets\/([^"]+)"/g)].map(m => m[1]);
  assert.ok(queue.length, 'index.html references no assets at all');
  const seen = new Set();
  while (queue.length) {
    const name = queue.shift();
    if (seen.has(name)) continue;
    seen.add(name);
    const file = path.join(assetsDir, name);
    assert.ok(existsSync(file),
      `board-dist references assets/${name}, which is not on disk — the rebuilt asset was never staged`);
    if (!/\.(?:js|css|html)$/.test(name)) continue;
    const src = readFileSync(file, 'utf8');
    // Quoted relative specifiers cover both lazy JS (import("./chunk.js")) and
    // Vite's dependency map entries ("./TermPane-<hash>.css").
    for (const m of src.matchAll(/["'`]\.\/([\w.-]+)["'`]/g)) {
      if (ASSET.test(m[1])) queue.push(m[1]);
    }
  }
  assert.ok([...seen].some(n => n.endsWith('.css')),
    'the graph walk found no stylesheet — the lazy CSS dependency map went unchecked');
  for (const extra of readdirSync(assetsDir)) {
    assert.ok(seen.has(extra),
      `board-dist/assets/${extra} is on disk but nothing references it — a stale build leftover`);
  }
});

// --- 0.19.3: a copy that cannot lie, and a gesture that teaches itself -------
//
// 0.19.2 shipped the copy chord and the user still could not paste: the pane
// said "✓ copied" over a clipboard that never changed. Neither clipboard API
// can be believed on its own — writeText() resolves when the write is ACCEPTED
// (Chrome may drop it afterwards) and execCommand('copy') returns true for
// "the command ran". Only the copy EVENT proves anything: if our listener fired,
// the browser took our string. These pin that the proof is what gets reported,
// and that the failure paths stay honest.

test('copyText proves the copy through a real copy event, not a return value', () => {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'util.js'), 'utf8');
  assert.match(src, /addEventListener\('copy', onCopy, true\)/,
    'copyText no longer drives a real copy event — its success is unprovable again');
  assert.match(src, /ok: ran && fired/,
    'copyText must report the LISTENER firing, not execCommand\'s return value');
  // The event path has to run before the async API, or an accepted-then-dropped
  // writeText() short-circuits the only path that can prove itself.
  const viaEvent = src.indexOf('const attempt = copyViaEvent(text)');
  const viaApi = src.indexOf('navigator.clipboard?.writeText');
  assert.ok(viaEvent > 0 && viaEvent < viaApi,
    'copyViaEvent must be attempted BEFORE navigator.clipboard.writeText');
  assert.match(src, /active\?\.focus\?\.\(\)/,
    'the fallback textarea steals focus; copyText must hand the keyboard back');
});

test('TermPane teaches the failed gesture and never nags over a working one', () => {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  // The hint fires ONLY when a modifier would have changed the outcome: the app
  // is tracking the mouse AND the sweep selected nothing. Without the first
  // condition it would fire over drags that selected fine.
  assert.match(src, /if \(mouseModeOn\(\) && !term\.hasSelection\(\)\) flash\('hint'/,
    'TermPane lost the guarded select hint');
  assert.match(src, /term\.modes\?\.mouseTrackingMode/,
    'mouse-mode detection must use xterm\'s public modes API');
  assert.ok(src.includes('DRAG_SLOP'), 'a click is not a failed selection — the slop threshold is load-bearing');
  // A failed copy must keep the selection: it is the only copy the human has
  // left, and right-click → Copy needs it on screen.
  const errBranch = src.slice(src.indexOf("flash('err', 'the clipboard refused"));
  assert.ok(!/clearSelection/.test(errBranch.slice(0, 200)),
    'a refused copy must NOT clear the selection');
});

test('a pane refused at the upgrade says so instead of "connection closed"', () => {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  assert.match(src, /st\.seen/, 'TermPane no longer tracks whether any frame arrived');
  assert.match(src, /hasToken\(\)\s*\?/,
    'the close path must distinguish "no board key" from "the daemon refused me"');
  assert.match(src, /this board has no key/,
    'a keyless board must name the actual problem — /ws/term is the only gated loopback route');
});

test('the daemon makes an upgrade impossible to miss and assets free to cache', () => {
  const src = readFileSync(path.join(HERE, '..', 'scripts', 'fleetd', 'http.mjs'), 'utf8');
  assert.match(src, /'cache-control': ext === '\.html' \? 'no-store' : 'public, max-age=31536000, immutable'/,
    'board assets lost their cache contract — a browser may serve a stale shell after an upgrade');
});

test('a copy that cannot be proven leaves evidence behind', () => {
  // "It said copied and my clipboard is unchanged" is undebuggable from the
  // outside — a page cannot read the OS clipboard — so every attempt records
  // which link of the chain reported what, and an unproven one is loud.
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'util.js'), 'utf8');
  assert.match(src, /globalThis\.__fdCopy = trace/,
    'the last copy attempt must stay inspectable — it is what to ask a reporter for');
  assert.match(src, /console\.warn\('\[fleetdeck\] copy could not be proven/,
    'an unprovable copy must say so in the console');
  // The read-back is the only real proof of arrival, but clipboard-read PROMPTS
  // in Chrome — a diagnostic may never put a permission dialog in front of
  // someone who just pressed Ctrl+C.
  assert.match(src, /perm\?\.state !== 'granted'/,
    'the clipboard read-back must be skipped unless the permission is ALREADY granted');
});

test('a granted read-back that PROVES a mismatch must veto the writeText fallback', async () => {
  // BUG-069: writeText() resolving means the write was ACCEPTED, not that the
  // clipboard changed — Chrome can drop it afterwards. When clipboard-read is
  // already granted, the read-back settles the question, and a proven mismatch
  // must report failure so TermPane keeps the selection instead of flashing
  // "✓ copied" over a stale clipboard. Drive copyText for real with a stubbed
  // DOM/navigator: no execCommand, a writeText that resolves, a readText that
  // returns something else.
  // Node ≥21 defines a global `navigator` getter — plain assignment throws, so
  // stub with defineProperty (configurable) and restore the originals after.
  const real = {
    navigator: Object.getOwnPropertyDescriptor(globalThis, 'navigator'),
    document: Object.getOwnPropertyDescriptor(globalThis, 'document'),
    window: Object.getOwnPropertyDescriptor(globalThis, 'window'),
  };
  const stub = (name, value) => Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
  stub('navigator', {
    permissions: { query: async () => ({ state: 'granted' }) },
    clipboard: {
      writeText: async () => {}, // accepted — but never lands
      readText: async () => 'something pasted from another app an hour earlier',
    },
  });
  stub('document', undefined); // no execCommand — the async fallback runs
  stub('window', undefined);
  try {
    assert.equal(await copyText('the text that was meant to be copied'), false,
      'a read-back mismatch must NOT be reported as copied');
    assert.match(globalThis.__fdCopy.verified, /^NO —/,
      'the trace must record the proven mismatch');
  } finally {
    for (const [k, d] of Object.entries(real)) {
      if (d) Object.defineProperty(globalThis, k, d); else delete globalThis[k];
    }
  }
});

// --- tmux passthrough: the agent's own clipboard write ----------------------
//
// THE bug behind three rounds of "copy still does not work". Claude Code's TUI
// owns the mouse, so the human's drag never reaches xterm; the TUI selects,
// prints its own "copied N characters", and writes the clipboard with OSC 52 —
// wrapped for tmux. Fleet Deck's client is tmux CONTROL MODE, which is not a
// terminal, so tmux never unwraps it: the board receives ESC P tmux ; … ESC \\
// intact, xterm sees an unknown DCS, and the whole clipboard write is discarded.
// Confirmed on the wire 2026-07-25 (raw OSC 52 arrives bare; the wrapped form
// arrives with its wrapper still on).

const E = String.fromCharCode(27);
const OSC52 = `${E}]52;c;SGVsbG8=${E === null ? '' : String.fromCharCode(7)}`;
const wrap = (inner) => `${E}Ptmux;${inner.split(E).join(E + E)}${E}\\`;

test('an agent clipboard write inside a tmux wrapper reaches the terminal', () => {
  const { out, carry } = unwrapTmuxPassthrough(`before${wrap(OSC52)}after`);
  assert.equal(out, `before${OSC52}after`, 'the OSC 52 must survive, unwrapped and un-doubled');
  assert.equal(carry, '');
});

test('only OSC 52 is unwrapped — passthrough is not a general licence', () => {
  // A pane renders bytes from files, tools and the network. Honouring arbitrary
  // passthrough would hand every one of them a direct line to the emulator.
  const evil = `${E}]0;retitled by a file you cat'd${String.fromCharCode(7)}`;
  const { out } = unwrapTmuxPassthrough(`a${wrap(evil)}b`);
  assert.equal(out, 'ab', 'a non-clipboard passthrough must be dropped, not forwarded');
});

test('a wrapper split across two frames is not lost', () => {
  const whole = `x${wrap(OSC52)}y`;
  for (const cut of [3, 9, 14, 20, whole.length - 2]) {
    const first = unwrapTmuxPassthrough(whole.slice(0, cut));
    const second = unwrapTmuxPassthrough(whole.slice(cut), first.carry);
    assert.equal(first.out + second.out, `x${OSC52}y`, `split at ${cut} lost data`);
    assert.equal(second.carry, '', `split at ${cut} left a dangling carry`);
  }
});

test('ordinary pane output is passed through untouched', () => {
  const plain = `${E}[1mbold${E}[0m rows and \r\n newlines`;
  assert.deepEqual(unwrapTmuxPassthrough(plain), { out: plain, carry: '' });
  // A lone ESC at a frame boundary must not be mistaken for a wrapper opening.
  assert.deepEqual(unwrapTmuxPassthrough('tail'), { out: 'tail', carry: '' });
});

// --- the OSC 52 provider answers to the pane's focus -------------------------
//
// BUG-084: the clipboard provider was module-scoped and write-capable on EVERY
// mounted tile, so output in an unfocused (watch-only) grid tile could silently
// replace the operator's clipboard — with attacker-chosen text if any agent,
// file or fetched page in the stream emitted OSC 52. These exercise the
// provider's exact shape, pulled out of TermPane.jsx so the pattern stays
// pinned to the source (the negative cases below FAIL against the old
// module-scoped provider, which wrote unconditionally and had no gate to stub).

function extractClipboardProvider(src) {
  // From OSC52_MAX (the provider's cap constant, defined just above it) so the
  // slice is self-contained.
  const start = src.indexOf('const OSC52_MAX = ');
  const end = src.indexOf('function cssVar(', start);
  assert.ok(start > 0 && end > start, 'TermPane.jsx no longer defines OSC52_MAX/clipboardProvider before cssVar');
  // copyText is the one free identifier the factory closes over — inject a spy.
  const spy = 'const copyText = globalThis.__copySpy;';
  return new Function(`${spy}\n${src.slice(start, end)}\nreturn clipboardProvider;`)();
}

function loadProvider() {
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  return { src, provider: extractClipboardProvider(src) };
}

test('OSC 52 writes are honoured on the live pane and refused on a watch-only one', async () => {
  const calls = [];
  globalThis.__copySpy = async (d) => { calls.push(d); return true; };
  const { src, provider } = loadProvider();

  // A factory handed the pane's own term, and loaded with it: the provider
  // must read the LIVE stdin flag, not a `live` prop frozen at mount (focus
  // flips are applied in place — the effect holding this provider never
  // re-runs, so a captured prop would go stale the moment focus moved).
  assert.match(src, /clipboardProvider\s*=\s*\(term\)\s*=>/,
    'the provider must take the pane term — a module-scoped one cannot see focus');
  assert.match(src, /new ClipboardAddon\(undefined, clipboardProvider\(term\)\)/,
    'the addon must be loaded with THIS pane\'s term');

  const term = { options: { disableStdin: true } }; // a watch-only tile
  const p = provider(term);
  await p.writeText('c', 'rm -rf ~ && ');
  assert.deepEqual(calls, [], 'a watch-only pane\'s OSC 52 must never reach the clipboard');

  // Focus moves TO the tile (the in-place effect clears disableStdin) and the
  // SAME provider now honours the write — a provider snapshotting `live` at
  // mount could never do this.
  term.options.disableStdin = false;
  await p.writeText('c', 'the human\'s own copy');
  assert.deepEqual(calls, ["the human's own copy"], 'the live pane\'s OSC 52 must reach the clipboard');

  // ...and away again (an ended pane sets disableStdin too — covered by the
  // same flag).
  term.options.disableStdin = true;
  await p.writeText('c', 'one more from a dead pane');
  assert.deepEqual(calls, ["the human's own copy"], 'a pane that ended must stop writing the clipboard');
});

test('OSC 52 reads stay one-way and writes are size-capped', async () => {
  const calls = [];
  globalThis.__copySpy = async (d) => { calls.push(d); return true; };
  const { src, provider } = loadProvider();

  const p = provider({ options: { disableStdin: false } });
  assert.equal(await p.readText(), '',
    'OSC 52\'s read form must answer nothing — honouring it exfiltrates the clipboard into stdin');

  assert.match(src, /data\.length > OSC52_MAX/, 'the provider lost its size cap');
  const big = 'x'.repeat(64 * 1024 + 1);
  await p.writeText('c', big);
  assert.deepEqual(calls, [], 'an oversized OSC 52 payload must be refused');
  await p.writeText('c', 'x'.repeat(64 * 1024));
  assert.equal(calls.length, 1, 'a payload at the cap is still a legitimate copy');
});

// --- pasting INTO a pane -----------------------------------------------------
//
// In a terminal Ctrl+V is not paste — it is the byte ^V — because the terminal
// and the program share a machine. Here they do not: the clipboard is in a
// browser, and Claude Code answers ^V by hunting for an image on the DAEMON
// HOST's clipboard, then saying "no image found". Truthful and useless, so the
// board claims the chord.

test('Ctrl+V is the paste chord off a Mac, and nothing to claim on one', () => {
  const down = { type: 'keydown', key: 'v' };
  assert.equal(isTermPasteChord({ ...down, ctrlKey: true }, false), true);
  // ⌘V is already the browser's own paste and xterm never intercepts meta.
  assert.equal(isTermPasteChord({ ...down, metaKey: true }, true), false);
  assert.equal(isTermPasteChord({ ...down, ctrlKey: true }, true), false);
  // Ctrl+Shift+V is the terminal's own paste and already works — leave it.
  assert.equal(isTermPasteChord({ ...down, ctrlKey: true, shiftKey: true }, false), false);
  assert.equal(isTermPasteChord({ ...down, ctrlKey: true, altKey: true }, false), false);
  assert.equal(isTermPasteChord({ ...down }, false), false, 'bare v must type a v');
  assert.equal(isTermPasteChord({ type: 'keyup', key: 'v', ctrlKey: true }, false), false);
});

test('the paste chord is surrendered to the BROWSER, never preventDefaulted', () => {
  // The whole mechanism: stop xterm turning Ctrl+V into ^V, but leave the event
  // alone so the browser performs its own TRUSTED paste. That needs no
  // clipboard-read permission, and the resulting paste event reaches xterm's
  // handler, which brackets it — without that, a multi-line paste submits
  // itself line by line into a live agent.
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  const line = src.split('\n').find(l => l.includes('isTermPasteChord(e'));
  assert.ok(line, 'TermPane no longer claims the paste chord');
  assert.ok(/return false;\s*$/.test(line.trim()),
    'the paste chord must return false ALONE — a preventDefault here kills the browser paste');
  assert.ok(!/preventDefault/.test(line), 'the paste chord must not preventDefault');
});

test('the headless image paste survives the Ctrl+V change', () => {
  // Pasting a screenshot uploads it to the DAEMON's filesystem and types the
  // path into the pane — the whole reason the feature exists on a headless
  // server, where the agent can read a file but never a clipboard. It rides the
  // browser's native paste event, which is exactly what the Ctrl+V handler
  // preserves by not calling preventDefault. Verified end to end 2026-07-25:
  // Ctrl+V delivers clipboardData.types ["Files"] and the path lands.
  const src = readFileSync(path.join(HERE, '..', 'board', 'src', 'components', 'TermPane.jsx'), 'utf8');
  assert.match(src, /screenEl\.addEventListener\('paste', onPaste, true\)/,
    'the image-paste listener must stay in the CAPTURE phase — xterm handles paste on the textarea below it');
  assert.match(src, /if \(!item\) \{/,
    'a text paste must enter the text branch and fall through to xterm when safe');
  assert.match(src, /sendIn\(res\.json\.path \+ ' '\)/,
    'the uploaded image must reach the pane as a PATH — the agent cannot read a clipboard');
  assert.ok(src.includes('press Enter to send'),
    'a paste must never submit on its own: keystrokes into a live agent are irreversible');
});
