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
  batchTotal,
  expandBatchTasks,
  modelFamily,
  modelShort,
  parseBatchTasks,
  prettyModel,
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
