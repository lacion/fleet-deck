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
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  MODEL_FAMILIES,
  imageFromClipboard,
  batchTotal,
  expandBatchTasks,
  modelFamily,
  modelShort,
  parseBatchTasks,
  prettyModel,
  sessionTicker,
} from '../board/src/util.js';

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
