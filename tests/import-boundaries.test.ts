// tests/import-boundaries.test.ts
//
// Architecture tripwire for the three module zones the Bun-primary layout pins:
//
//   contracts/     — the shared type/validator spine (imported by everyone)
//   src/daemon/    — the single-runtime daemon (bun:sqlite, Bun.serve, and the
//                    daemon-only deps effect/kysely/hono when they land)
//   board/src/     — the Vite SPA (browser bundle; no daemon runtime, no bun:*)
//   scripts/fleet-*.ts — the FAIL-OPEN hook floor (must stay a thin edge onto a
//                    named seam of src/daemon, never drag the whole server in)
//
// These are grep-level invariants over the *actual* sanctioned edges as they
// exist today (see the discovery in R2): the daemon never reaches up into the
// UI; the UI never reaches down into the daemon or its runtime-only deps; the
// fail-open floor touches src/daemon only through the four seam modules it
// already imports. Biome's noRestrictedImports (Step 1 / R3) later graduates
// the SAME edges to a lint rule, but a suite test is the stronger anchor — lint
// can be silenced per-line; a green-or-revert test cannot ([[test-suite-is-trust]]).
//
// The predicates are pure and self-validating: the final `meta` test drives
// synthetic (importer, specifier) pairs through the exact functions the file
// walk uses, asserting they BOTH catch real violations and clear the legit
// edges — so this tripwire provably can trip. A boundary test that can never
// fail is worse than none.

import test from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const DAEMON = path.join(REPO, 'src', 'daemon');
const BOARD = path.join(REPO, 'board');
const BOARD_SRC = path.join(REPO, 'board', 'src');

// The daemon modules the fail-open floor (scripts/fleet-*.ts) is sanctioned to
// import — the config/nonce/env-scrub/takeover seam it uses to agree with a
// running daemon. Growing this set is a deliberate act: widening the floor's
// dependency surface must be a conscious edit here, which is the whole point.
const FLOOR_FILES = ['fleet-hook.ts', 'fleet-sessionstart.ts', 'fleet-watch.ts'].map((f) =>
  path.join(REPO, 'scripts', f),
);
const FLOOR_SEAM_ALLOW = new Set(['config', 'run-nonce', 'env-scrub', 'takeover']);

// Runtime-only deps that belong to the daemon and must never reach the browser
// bundle. `effect` is the named board tripwire ([[drive-and-observe-provider]]);
// kysely/hono are the other Step 4/5 daemon deps; `bun:*` builtins are
// nonsensical (and unresolvable) under Vite.
function isDaemonOnlyDep(bare: string): boolean {
  if (bare.startsWith('bun:')) return true;
  for (const dep of ['effect', 'kysely', 'hono']) {
    if (bare === dep || bare.startsWith(`${dep}/`)) return true;
  }
  return false;
}

// Recursively collect .ts/.tsx SOURCE under a root, skipping generated/vendored
// trees (board-dist is built board assets that live under src/daemon).
function walkSources(root: string): string[] {
  const out: string[] = [];
  const skip = new Set(['node_modules', 'board-dist', 'dist', '.git']);
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop() as string;
    let entries: fs.Dirent[];
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const full = path.join(dir, e.name);
      if (e.isDirectory()) {
        if (!skip.has(e.name)) stack.push(full);
      } else if (e.isFile() && /\.(ts|tsx)$/.test(e.name)) {
        out.push(full);
      }
    }
  }
  return out;
}

// Every static module specifier a source file names: `import … from '…'`,
// `export … from '…'`, dynamic `import('…')`, and bare side-effect `import '…'`.
// The `from '…'` clause sits on its own line even for multi-line binding lists,
// so a whole-file scan catches it regardless of wrapping. Computed dynamic
// imports (non-literal argument) are intentionally out of scope — a static
// tripwire pins static edges.
function specifiersOf(source: string): string[] {
  const specs: string[] = [];
  const patterns = [
    /\bfrom\s*['"]([^'"]+)['"]/g,
    /\bimport\s*\(\s*['"]([^'"]+)['"]\s*\)/g,
    /(?:^|[;{}\n])\s*import\s+['"]([^'"]+)['"]/g,
  ];
  for (const re of patterns) {
    for (const m of source.matchAll(re)) {
      const spec = m[1];
      if (spec !== undefined) specs.push(spec);
    }
  }
  return specs;
}

function isUnder(abs: string, root: string): boolean {
  return abs === root || abs.startsWith(root + path.sep);
}

// Resolve a specifier relative to the importing file. Relative specifiers yield
// an absolute path; bare specifiers (packages / bun: builtins) yield the raw
// name. A discriminated union so each predicate narrows without a null dance.
type Resolved = { kind: 'rel'; abs: string } | { kind: 'bare'; bare: string };
function resolveSpec(fromFile: string, spec: string): Resolved {
  if (spec.startsWith('.')) return { kind: 'rel', abs: path.resolve(path.dirname(fromFile), spec) };
  return { kind: 'bare', bare: spec };
}

// --- the three boundary predicates (shared by the walk tests and the meta test) ---

// src/daemon/** must never import up into board/**.
function daemonImportsBoard(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  return r.kind === 'rel' && isUnder(r.abs, BOARD);
}

// board/src/** must never import src/daemon/** nor any daemon-only dep.
function boardImportsDaemon(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  if (r.kind === 'rel') return isUnder(r.abs, DAEMON);
  return isDaemonOnlyDep(r.bare);
}

// scripts/fleet-*.ts may reach into src/daemon only via the seam allowlist.
function floorImportsForbiddenDaemon(fromFile: string, spec: string): boolean {
  const r = resolveSpec(fromFile, spec);
  if (r.kind !== 'rel' || !isUnder(r.abs, DAEMON)) return false;
  const base = path.basename(r.abs).replace(/\.tsx?$/, '');
  return !FLOOR_SEAM_ALLOW.has(base);
}

function scan(files: string[], predicate: (f: string, s: string) => boolean): string[] {
  const violations: string[] = [];
  for (const file of files) {
    const source = fs.readFileSync(file, 'utf8');
    for (const spec of specifiersOf(source)) {
      if (predicate(file, spec)) violations.push(`${path.relative(REPO, file)} → ${spec}`);
    }
  }
  return violations;
}

test('boundary: src/daemon never imports board/**', () => {
  const violations = scan(walkSources(DAEMON), daemonImportsBoard);
  assert.deepEqual(
    violations,
    [],
    `the daemon must not depend on the UI:\n${violations.join('\n')}`,
  );
});

test('boundary: board/src never imports src/daemon/** or a daemon-only dep', () => {
  const violations = scan(walkSources(BOARD_SRC), boardImportsDaemon);
  assert.deepEqual(
    violations,
    [],
    `the board talks to the daemon over HTTP, never by import (and never pulls effect/kysely/hono/bun:*):\n${violations.join('\n')}`,
  );
});

test('boundary: the fail-open floor imports src/daemon only through the seam allowlist', () => {
  const violations = scan(FLOOR_FILES, floorImportsForbiddenDaemon);
  assert.deepEqual(
    violations,
    [],
    `scripts/fleet-*.ts must stay a thin edge onto {${[...FLOOR_SEAM_ALLOW].join(', ')}}; widening the seam is a deliberate edit here:\n${violations.join('\n')}`,
  );
});

// The tripwire must be able to trip: drive known-bad and known-good pairs
// through the exact predicates the walk uses.
test('boundary: the predicates catch violations and clear the sanctioned edges', () => {
  const daemonFile = path.join(DAEMON, 'http.ts');
  const boardFile = path.join(BOARD_SRC, 'components', 'Header.tsx');
  const floorFile = path.join(REPO, 'scripts', 'fleet-hook.ts');

  // daemon → board
  assert.equal(daemonImportsBoard(daemonFile, '../../board/src/util.ts'), true);
  assert.equal(daemonImportsBoard(daemonFile, './db.ts'), false);
  assert.equal(daemonImportsBoard(daemonFile, '../../contracts/index.ts'), false);
  assert.equal(daemonImportsBoard(daemonFile, '../../bin/tmux-version.ts'), false);

  // board → daemon / daemon-only dep
  assert.equal(boardImportsDaemon(boardFile, '../../../src/daemon/db.ts'), true);
  assert.equal(boardImportsDaemon(boardFile, 'effect'), true);
  assert.equal(boardImportsDaemon(boardFile, 'effect/Schema'), true);
  assert.equal(boardImportsDaemon(boardFile, 'bun:sqlite'), true);
  assert.equal(boardImportsDaemon(boardFile, '../../../contracts/index.ts'), false);
  assert.equal(boardImportsDaemon(boardFile, '../util.ts'), false);
  assert.equal(boardImportsDaemon(boardFile, 'react'), false);

  // floor → daemon seam
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/http.ts'), true);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/db.ts'), true);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/config.ts'), false);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../src/daemon/takeover.ts'), false);
  assert.equal(floorImportsForbiddenDaemon(floorFile, '../contracts/index.ts'), false);
});
