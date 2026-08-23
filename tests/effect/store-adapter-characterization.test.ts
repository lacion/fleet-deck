// store-adapter-characterization.test.ts — P8.1 characterization.
//
// This file pins the observable contract of the Bun SQLite adapter
// (src/daemon/sqlite.ts) so that removing the stale dynamic node:sqlite fallback
// and importing bun:sqlite statically is provably behavior-neutral on Bun.
//
// The daemon is Bun-only since 0.23.0: bin/fleetdeck.ts's serve() preflight
// exits EXIT_WRONG_RUNTIME (78, EX_CONFIG) before it imports the daemon bundle
// whenever process.versions.bun is absent or below MIN_BUN_VERSION (1.3.14), and
// CI runs ONE authoritative Bun test lane (.github/workflows/ci.yml — no
// `node --test` job). So the node branch of the seam is dead code on every
// gated path, and the four behavioral tests below hold IDENTICALLY before and
// after the removal — they are the behavior-neutrality proof (run the file 3x
// before and 3x after; the four behavioral outcomes never change).
//
// The seam's selection logic BEFORE this change, pinned verbatim so the removal
// diff is auditable:
//
//     let makeHandle: (file: string) => SqliteHandle;
//     if (process.versions.bun) {
//       const { Database } = await import('bun:sqlite');
//       makeHandle = (file) => wrap(new Database(file));
//     } else {
//       // node:sqlite ExperimentalWarning filter (fleetdSqliteWarningFilter) ...
//       const { DatabaseSync } = await import('node:sqlite');
//       makeHandle = (file) => wrap(new DatabaseSync(file));
//     }
//
// On Bun the guard picks the `bun:sqlite` arm — `wrap(new Database(file))` — and
// that is exactly what remains after the change. The node arm was reachable only
// when process.versions.bun was falsy, which the preflight and the single Bun CI
// lane make impossible in production and in the gate. Test #2 pins that Bun
// outcome behaviorally; the structural test (#5) captures the selection logic's
// source shape so the deletion is a reviewed diff rather than a silent one.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Database } from 'bun:sqlite';
import {
  openDatabase,
  type SqliteHandle,
  type SqlRunResult,
  type SqlValue,
} from '../../src/daemon/sqlite.ts';
import test from '../helpers/harness-test.ts';

const SQLITE_TS = fileURLToPath(new URL('../../src/daemon/sqlite.ts', import.meta.url));

function withDb(fn: (db: SqliteHandle) => void): void {
  const db = openDatabase(':memory:');
  try {
    fn(db);
  } finally {
    db.close();
  }
}

// (1) Precondition: the daemon and this test process run under Bun, which is why
// the node:sqlite arm is dead code and safe to delete.
test('characterization: the daemon test runtime is Bun (the node:sqlite arm is dead code)', () => {
  assert.ok(
    process.versions.bun,
    'fleetd is Bun-only since 0.23.0 (serve() preflight exits 78 on non-Bun); tests run under Bun too',
  );
});

// (2) The seam wraps bun:sqlite specifically. Proven by divergence: the raw
// bun:sqlite driver returns `null` for a missed .get(), while openDatabase()
// returns `undefined`. The null->undefined normalization is only OBSERVABLE as
// active over a driver that yields null (bun:sqlite); node:sqlite already yields
// undefined, so this doubles as proof the underlying driver is bun:sqlite — i.e.
// the selection logic's Bun outcome. This assertion is byte-identical before and
// after the change (both resolve to `wrap(new Database(file))`).
test('characterization: openDatabase wraps bun:sqlite — raw driver yields null on a miss, the seam normalizes to undefined', () => {
  const raw = new Database(':memory:');
  try {
    raw.exec('CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)');
    assert.strictEqual(
      raw.prepare('SELECT * FROM t WHERE id = ?').get(999),
      null,
      'raw bun:sqlite returns null on a missed row — the one divergence the seam normalizes',
    );
  } finally {
    raw.close();
  }

  withDb((db) => {
    db.exec('CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)');
    assert.strictEqual(
      db.prepare('SELECT * FROM t WHERE id = ?').get(999),
      undefined,
      'the seam pins a missed row to undefined (node:sqlite parity), over a bun:sqlite driver',
    );
  });
});

// (3) null->undefined normalization on EVERY read path: a missed .get() ->
// undefined, a hit .get() -> the row, an empty .all() -> [], a populated .all()
// -> the rows. Pins the exact miss sentinel the rest of fleetd reads with ?? /
// ?. / truthiness.
test('characterization: normalization holds on every read path (.get miss/hit, .all empty/rows)', () => {
  withDb((db) => {
    db.exec('CREATE TABLE item(id INTEGER PRIMARY KEY, name TEXT NOT NULL)');
    const insert = db.prepare('INSERT INTO item(name) VALUES (?)');
    insert.run('heron');
    insert.run('egret');

    const byId = db.prepare<{ id: number; name: string }>('SELECT id, name FROM item WHERE id = ?');
    assert.strictEqual(byId.get(404), undefined, 'a missed .get() is undefined, never null');
    assert.deepEqual(byId.get(1), { id: 1, name: 'heron' }, 'a hit .get() returns the row object');

    const all = db.prepare<{ id: number; name: string }>('SELECT id, name FROM item ORDER BY id');
    assert.deepEqual(
      all.all(),
      [
        { id: 1, name: 'heron' },
        { id: 2, name: 'egret' },
      ],
      'a populated .all() returns every row in order',
    );
    assert.deepEqual(
      db.prepare('SELECT id FROM item WHERE id > 100').all(),
      [],
      'an empty .all() is [], not null/undefined',
    );
  });
});

// (4) The public row-type runtime shape. A present row is a column-keyed plain
// object whose cell values are the SqlValue storage classes: INTEGER -> number,
// TEXT -> string, REAL -> number, BLOB -> Uint8Array, and a NULL COLUMN -> null.
// The null-column case is the mirror of #3: a null CELL in a present row stays
// null (SqlValue includes null); only a missed ROW becomes undefined. .run()
// reports SqlRunResult { changes, lastInsertRowid } as JS numbers here.
test('characterization: public row types — SqlValue storage classes and SqlRunResult numeric shape', () => {
  withDb((db) => {
    db.exec(
      'CREATE TABLE shapes(id INTEGER PRIMARY KEY, n INTEGER, t TEXT, r REAL, b BLOB, empty TEXT)',
    );
    const blob = new Uint8Array([0xde, 0xad, 0xbe, 0xef]);
    const result: SqlRunResult = db
      .prepare('INSERT INTO shapes(n, t, r, b, empty) VALUES (?, ?, ?, ?, ?)')
      .run(7, 'name', 1.5, blob, null);

    assert.strictEqual(result.changes, 1, 'SqlRunResult.changes counts the affected row');
    assert.strictEqual(typeof result.changes, 'number', 'changes is a JS number for small counts');
    assert.strictEqual(result.lastInsertRowid, 1, 'SqlRunResult.lastInsertRowid is the new rowid');
    assert.strictEqual(
      typeof result.lastInsertRowid,
      'number',
      'lastInsertRowid is a JS number for small rowids',
    );

    const row = db.prepare<Record<string, SqlValue>>('SELECT * FROM shapes WHERE id = ?').get(1);
    assert.ok(row, 'the inserted row is present');
    assert.deepEqual(
      row,
      { id: 1, n: 7, t: 'name', r: 1.5, b: blob, empty: null },
      'a present row is a column-keyed object: INTEGER->number, TEXT->string, REAL->number, BLOB->Uint8Array, NULL column->null',
    );
    assert.ok(row.b instanceof Uint8Array, 'a BLOB cell round-trips as a Uint8Array');
    assert.strictEqual(row.empty, null, 'a NULL cell in a present row stays null (not undefined)');
  });
});

// (5) Selection-logic structural pin. Unlike #1-#4 (the permanent
// behavior-neutrality proof, identical before and after), this test
// characterizes the code that was CHANGED, so its assertions moved with the
// diff. It was authored asserting the pre-change dual-runtime selection logic
// (an `if (process.versions.bun)` branch, `await import('bun:sqlite')` /
// `await import('node:sqlite')`, and the fleetdSqliteWarningFilter) — capturing
// the baseline that was deleted — and is now flipped to the post-change shape,
// where it guards against re-introducing the node:sqlite arm. The negatives
// target CODE constructs (DatabaseSync, `await import(`, the `if` branch, the
// warning filter), not the header's historical prose, which still mentions the
// retired arm for context. Kept last and clearly labeled.
test('characterization: the seam selects its SQLite driver by a static bun:sqlite import (no node:sqlite fallback)', () => {
  const source = readFileSync(SQLITE_TS, 'utf8');
  assert.match(
    source,
    /^import \{ Database \} from 'bun:sqlite';$/m,
    'sqlite.ts imports bun:sqlite statically at the top level',
  );
  assert.doesNotMatch(
    source,
    /await import\(/,
    'no dynamic driver import remains — bun:sqlite is a static import',
  );
  assert.doesNotMatch(
    source,
    /if \(process\.versions\.bun\)/,
    'no runtime driver branch remains — the seam is single-runtime',
  );
  assert.doesNotMatch(
    source,
    /DatabaseSync/,
    'the node:sqlite driver (DatabaseSync) must not be re-introduced',
  );
  assert.doesNotMatch(
    source,
    /emitWarning|ExperimentalWarning/,
    'the node:sqlite ExperimentalWarning filter must not return',
  );
});
