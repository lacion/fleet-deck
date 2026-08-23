// sqlite-strict-trial.test.ts — P8.2 trial of bun:sqlite DatabaseOptions.strict at
// Bun 1.3.14. Fixtures open scratch DBs only (`:memory:` or mkdtemp); they never
// touch the daemon's store. Each behavioural fixture asserts BOTH strict:false
// and strict:true so the delta is the evidence.
//
// This file does NOT enable `strict` (or `safeIntegers`) on the daemon. The open
// seam in src/daemon/sqlite.ts stays `new Database(file)` with no options; a
// tripwire below pins that. See docs/v1/evidence/effect/p8-strict-trial.md.

import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { Database, type SQLQueryBindings, type Statement } from 'bun:sqlite';
import { describe, test } from 'bun:test';

const REPO_ROOT = path.resolve(import.meta.dir, '../..');
const SCHEMA = `CREATE TABLE t (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT,
  n INTEGER
);`;

interface Thrown {
  threw: true;
  name: string;
  message: string;
}
interface Succeeded<T> {
  threw: false;
  value: T;
}
type Outcome<T> = Thrown | Succeeded<T>;

interface NameRow {
  name: string | null;
}
interface PairRow {
  name: string | null;
  n: number | bigint | null;
}
interface IntRow {
  n: string | number | bigint | null;
  t: string;
}

function capture<T>(fn: () => T): Outcome<T> {
  try {
    return { threw: false, value: fn() };
  } catch (err) {
    return {
      threw: true,
      name: err instanceof Error ? err.name : 'Error',
      message: err instanceof Error ? err.message : String(err),
    };
  }
}

function qprepare<T>(db: Database, sql: string): Statement<T, SQLQueryBindings[]> {
  return db.prepare(sql) as Statement<T, SQLQueryBindings[]>;
}

function runUnchecked(
  stmt: { run: (...args: never[]) => { changes: number; lastInsertRowid: number | bigint } },
  ...args: unknown[]
): { changes: number; lastInsertRowid: number | bigint } {
  return stmt.run(...(args as never[]));
}

function open(strict: boolean): Database {
  const db = new Database(':memory:', { strict });
  db.exec(SCHEMA);
  return db;
}

function withModes(fn: (db: Database, strict: boolean) => void): void {
  for (const strict of [false, true] as const) {
    const db = open(strict);
    try {
      fn(db, strict);
    } finally {
      db.close();
    }
  }
}

function withFileModes(fn: (db: Database, strict: boolean) => void): void {
  for (const strict of [false, true] as const) {
    const dir = mkdtempSync(path.join(tmpdir(), 'fd-p8-strict-'));
    const db = new Database(path.join(dir, 'trial.db'), { strict });
    try {
      fn(db, strict);
    } finally {
      try {
        db.close();
      } catch {
        // close(true) may already have closed or refused; always drop the scratch dir.
      }
      rmSync(dir, { recursive: true, force: true });
    }
  }
}

function preparedSql(source: string): string[] {
  const out: string[] = [];
  const re = /\.prepare(?:<[^>]*>)?\(\s*(?:`([^`]*?)`|'([^']*)'|"([^"]*)")/g;
  let match: RegExpExecArray | null = re.exec(source);
  while (match) {
    const sql = match[1] ?? match[2] ?? match[3];
    if (sql !== undefined) out.push(sql);
    match = re.exec(source);
  }
  return out;
}

function namedPlaceholders(sql: string): string[] {
  return sql.match(/(?:[:$@])[A-Za-z_][A-Za-z0-9_]*/g) ?? [];
}

describe('P8.2 bun:sqlite DatabaseOptions.strict trial (Bun 1.3.14)', () => {
  test('the daemon seam still opens with no constructor options', () => {
    const source = readFileSync(path.join(REPO_ROOT, 'src/daemon/sqlite.ts'), 'utf8');
    assert.match(source, /makeHandle = \(file\) => wrap\(new Database\(file\)\);/);
    assert.doesNotMatch(source, /\bstrict\s*:/);
    assert.doesNotMatch(source, /\bsafeIntegers\b/);
    assert.match(source, /run\(\.\.\.params: SqlValue\[\]\): SqlRunResult;/);
  });

  test('argument-less new Database() matches { strict: false } named-bind convention', () => {
    const def = new Database(':memory:');
    const off = new Database(':memory:', { strict: false });
    for (const db of [def, off]) db.exec(SCHEMA);

    def.prepare('INSERT INTO t (name) VALUES ($name)').run({ $name: 'prefixed' });
    off.prepare('INSERT INTO t (name) VALUES ($name)').run({ $name: 'prefixed' });
    assert.deepEqual(qprepare<NameRow>(def, 'SELECT name FROM t').get(), { name: 'prefixed' });
    assert.deepEqual(qprepare<NameRow>(off, 'SELECT name FROM t').get(), { name: 'prefixed' });

    def.prepare('INSERT INTO t (name) VALUES ($name)').run({ name: 'bare' });
    off.prepare('INSERT INTO t (name) VALUES ($name)').run({ name: 'bare' });
    const defBare = qprepare<NameRow>(def, 'SELECT name FROM t ORDER BY id DESC').get();
    const offBare = qprepare<NameRow>(off, 'SELECT name FROM t ORDER BY id DESC').get();
    assert.deepEqual(defBare, { name: null });
    assert.deepEqual(offBare, { name: null });
    def.close();
    off.close();
  });

  test('{ strict: true } does not enable safeIntegers', () => {
    const db = open(true);
    const result = db.prepare('INSERT INTO t (n) VALUES (?)').run(7);
    const row = qprepare<{ id: number | bigint; n: number | bigint }>(
      db,
      'SELECT id, n FROM t',
    ).get();
    assert.equal(typeof result.lastInsertRowid, 'number');
    assert.equal(typeof result.changes, 'number');
    assert.equal(typeof row?.id, 'number');
    assert.equal(typeof row?.n, 'number');
    assert.equal(row?.n, 7);
    db.close();
  });

  test('positional rest-args (corpus style) bind identically in both modes', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name, n) VALUES (?, ?)');
      assert.equal(ins.paramsCount, 2, `strict=${strict}`);
      const result = ins.run('alpha', 3);
      assert.equal(result.changes, 1, `strict=${strict}`);
      assert.equal(typeof result.lastInsertRowid, 'number', `strict=${strict}`);
      const row = qprepare<PairRow>(db, 'SELECT name, n FROM t').get();
      assert.deepEqual(row, { name: 'alpha', n: 3 }, `strict=${strict}`);
    });
  });

  test('positional extra and partial throws are identical in both modes', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name, n) VALUES (?, ?)');
      const missing = capture(() => ins.run('only-one'));
      const extra = capture(() => runUnchecked(ins, 'a', 1, 2));
      assert.equal(missing.threw, true, `strict=${strict}`);
      assert.equal(extra.threw, true, `strict=${strict}`);
      if (!missing.threw || !extra.threw) return;
      assert.equal(
        missing.message,
        'SQLite query expected 2 values, received 1',
        `strict=${strict}`,
      );
      assert.equal(extra.message, 'SQLite query expected 2 values, received 3', `strict=${strict}`);
    });
  });

  test('positional missing-all (zero-arg) succeeds and stores NULL in both modes', () => {
    // Corpus never does this: every zero-arg q.* call is on a 0-placeholder statement.
    // strict:true does NOT tighten the missing-all positional path the corpus would
    // actually use if someone omitted rest-args on a parameterized INSERT.
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name, n) VALUES (?, ?)');
      const result = capture(() => ins.run());
      assert.equal(result.threw, false, `strict=${strict} zero-arg must not throw`);
      const row = qprepare<PairRow>(db, 'SELECT name, n FROM t').get();
      assert.deepEqual(row, { name: null, n: null }, `strict=${strict}`);
    });
  });

  test('omit-after-bind reuses the last values in both modes', () => {
    withModes((db, strict) => {
      db.prepare('INSERT INTO t (name, n) VALUES (?, ?)').run('kept', 9);
      const sel = qprepare<NameRow>(db, 'SELECT name FROM t WHERE n = ?');
      assert.deepEqual(sel.get(9), { name: 'kept' }, `strict=${strict}`);
      assert.deepEqual(sel.get(), { name: 'kept' }, `strict=${strict} reuse`);
    });
  });

  test('numbered ?1/?2 is positional in both modes', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name, n) VALUES (?1, ?2)');
      ins.run('num', 4);
      assert.deepEqual(
        qprepare<PairRow>(db, 'SELECT name, n FROM t').get(),
        { name: 'num', n: 4 },
        `strict=${strict}`,
      );
      const missing = capture(() => ins.run('only'));
      assert.equal(missing.threw, true, `strict=${strict}`);
      if (!missing.threw) return;
      assert.equal(
        missing.message,
        'SQLite query expected 2 values, received 1',
        `strict=${strict}`,
      );
    });
  });

  test('a single array argument expands as the positional list in both modes', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name, n) VALUES (?, ?)');
      runUnchecked(ins, ['arr', 5]);
      db.run('INSERT INTO t (name, n) VALUES (?, ?)', ['db-run', 6]);
      const rows = qprepare<PairRow>(db, 'SELECT name, n FROM t ORDER BY id').all();
      assert.deepEqual(
        rows,
        [
          { name: 'arr', n: 5 },
          { name: 'db-run', n: 6 },
        ],
        `strict=${strict}`,
      );
    });
  });

  test('object bind to positional ? is a no-op (NULL) unless strict, which throws', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name) VALUES (?)');
      const result = capture(() => runUnchecked(ins, { name: 'obj' }));
      const row = qprepare<NameRow>(db, 'SELECT name FROM t').get();
      if (strict) {
        assert.equal(result.threw, true);
        if (!result.threw) return;
        assert.equal(result.message, 'Missing parameter "1"');
        assert.equal(row, null);
      } else {
        assert.equal(result.threw, false);
        assert.deepEqual(row, { name: null });
      }
    });
  });

  test('named $name prefix vs bare keys invert across modes', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name) VALUES ($name)');
      const prefixed = capture(() => runUnchecked(ins, { $name: 'prefixed' }));
      const prefixedRow = qprepare<NameRow>(db, 'SELECT name FROM t ORDER BY id DESC').get();
      const bare = capture(() => runUnchecked(ins, { name: 'bare' }));
      const bareRow = qprepare<NameRow>(db, 'SELECT name FROM t ORDER BY id DESC').get();

      if (strict) {
        assert.equal(prefixed.threw, true);
        if (!prefixed.threw) return;
        assert.equal(prefixed.message, 'Missing parameter "name"');
        assert.equal(prefixedRow, null);
        assert.equal(bare.threw, false);
        assert.deepEqual(bareRow, { name: 'bare' });
      } else {
        assert.equal(prefixed.threw, false);
        assert.deepEqual(prefixedRow, { name: 'prefixed' });
        assert.equal(bare.threw, false);
        assert.deepEqual(bareRow, { name: null });
      }
    });
  });

  test('named :name and @name follow the same prefix-vs-bare inversion', () => {
    withModes((db, strict) => {
      for (const spec of [
        { sql: 'INSERT INTO t (name) VALUES (:name)', prefixKey: ':name' },
        { sql: 'INSERT INTO t (name) VALUES (@name)', prefixKey: '@name' },
      ] as const) {
        db.exec('DELETE FROM t');
        const ins = db.prepare(spec.sql);
        const prefixed = capture(() => runUnchecked(ins, { [spec.prefixKey]: 'prefixed' }));
        const prefixedRow = qprepare<NameRow>(db, 'SELECT name FROM t').get();
        db.exec('DELETE FROM t');
        const bare = capture(() => runUnchecked(ins, { name: 'bare' }));
        const bareRow = qprepare<NameRow>(db, 'SELECT name FROM t').get();

        if (strict) {
          assert.equal(prefixed.threw, true, spec.sql);
          if (!prefixed.threw) return;
          assert.equal(prefixed.message, 'Missing parameter "name"', spec.sql);
          assert.equal(prefixedRow, null, spec.sql);
          assert.equal(bare.threw, false, spec.sql);
          assert.deepEqual(bareRow, { name: 'bare' }, spec.sql);
        } else {
          assert.equal(prefixed.threw, false, spec.sql);
          assert.deepEqual(prefixedRow, { name: 'prefixed' }, spec.sql);
          assert.equal(bare.threw, false, spec.sql);
          assert.deepEqual(bareRow, { name: null }, spec.sql);
        }
      }
    });
  });

  test('missing named parameter: non-strict stores NULL, strict throws', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name) VALUES ($name)');
      const result = capture(() => runUnchecked(ins, { other: 'nope' }));
      const row = qprepare<NameRow>(db, 'SELECT name FROM t').get();
      if (strict) {
        assert.equal(result.threw, true);
        if (!result.threw) return;
        assert.equal(result.message, 'Missing parameter "name"');
        assert.equal(row, null);
      } else {
        assert.equal(result.threw, false);
        assert.deepEqual(row, { name: null });
      }
    });
  });

  test('extra named keys are ignored when the mode-required key is present', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (name) VALUES ($name)');
      const matching = strict ? { name: 'keep', extra: 1 } : { $name: 'keep', $extra: 1 };
      const result = capture(() => runUnchecked(ins, matching));
      assert.equal(result.threw, false, `strict=${strict}`);
      assert.deepEqual(
        qprepare<NameRow>(db, 'SELECT name FROM t').get(),
        { name: 'keep' },
        `strict=${strict}`,
      );
    });
  });

  test('null and undefined store NULL in both modes', () => {
    withModes((db, strict) => {
      db.prepare('INSERT INTO t (name, n) VALUES (?, ?)').run(null, 1);
      const fromNull = qprepare<IntRow>(
        db,
        'SELECT n, typeof(name) AS t FROM t WHERE id = 1',
      ).get();
      assert.equal(fromNull?.n, 1, `strict=${strict}`);
      assert.equal(fromNull?.t, 'null', `strict=${strict}`);

      runUnchecked(db.prepare('INSERT INTO t (name, n) VALUES (?, ?)'), undefined, 2);
      const fromUndef = qprepare<IntRow>(
        db,
        'SELECT n, typeof(name) AS t FROM t WHERE id = 2',
      ).get();
      assert.equal(fromUndef?.n, 2, `strict=${strict}`);
      assert.equal(fromUndef?.t, 'null', `strict=${strict}`);
    });
  });

  test('INTEGER timestamps and values past 2^31 round-trip as number in both modes', () => {
    const values = [2 ** 31 - 1, 2 ** 31, 2 ** 32, 1_700_000_000_000, Number.MAX_SAFE_INTEGER];
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (n) VALUES (?)');
      const sel = qprepare<{ n: number | bigint }>(
        db,
        'SELECT n FROM t WHERE id = last_insert_rowid()',
      );
      for (const value of values) {
        ins.run(value);
        const row = sel.get();
        assert.equal(typeof row?.n, 'number', `strict=${strict} value=${value}`);
        assert.equal(row?.n, value, `strict=${strict} value=${value}`);
      }
    });
  });

  test('bigint above 2^53 returns a truncated number in both modes (safeIntegers off)', () => {
    withModes((db, strict) => {
      const ins = db.prepare('INSERT INTO t (n) VALUES (?)');
      ins.run(2n ** 53n + 1n);
      const overSafe = qprepare<IntRow>(
        db,
        'SELECT n, typeof(n) AS t FROM t WHERE id = last_insert_rowid()',
      ).get();
      assert.equal(typeof overSafe?.n, 'number', `strict=${strict}`);
      assert.equal(overSafe?.n, 2 ** 53, `strict=${strict}`);
      assert.equal(overSafe?.t, 'integer', `strict=${strict}`);

      ins.run(2n ** 60n);
      const over = qprepare<IntRow>(
        db,
        'SELECT n, typeof(n) AS t FROM t WHERE id = last_insert_rowid()',
      ).get();
      assert.equal(typeof over?.n, 'number', `strict=${strict}`);
      assert.equal(over?.n, 1152921504606847000, `strict=${strict}`);
    });
  });

  test('safeIntegers:true is independent of strict and is the bigint landmine', () => {
    for (const strict of [false, true] as const) {
      const db = new Database(':memory:', { strict, safeIntegers: true });
      db.exec(SCHEMA);
      const result = db.prepare('INSERT INTO t (n) VALUES (?)').run(2n ** 60n);
      const row = qprepare<{ id: number | bigint; n: number | bigint }>(
        db,
        'SELECT id, n FROM t',
      ).get();
      assert.equal(typeof result.lastInsertRowid, 'bigint', `strict=${strict}`);
      assert.equal(typeof row?.id, 'bigint', `strict=${strict}`);
      assert.equal(typeof row?.n, 'bigint', `strict=${strict}`);
      assert.equal(row?.n, 2n ** 60n, `strict=${strict}`);
      db.close();
    }
  });

  test('prepare is uncached, query is cached, finalize is identical in both modes', () => {
    withModes((db, strict) => {
      const sql = 'SELECT 1 AS x';
      const preparedA = db.prepare(sql);
      const preparedB = db.prepare(sql);
      assert.notEqual(preparedA, preparedB, `strict=${strict} prepare identity`);

      const queriedA = db.query(sql);
      const queriedB = db.query(sql);
      assert.equal(queriedA, queriedB, `strict=${strict} query cache`);

      queriedA.finalize();
      const after = capture(() => queriedA.get());
      assert.equal(after.threw, true, `strict=${strict}`);
      if (!after.threw) return;
      assert.equal(after.message, 'Statement has finalized', `strict=${strict}`);

      queriedA.finalize();
      const queriedC = db.query(sql);
      assert.notEqual(queriedC, queriedA, `strict=${strict} cache miss after finalize`);
      assert.deepEqual(queriedC.get(), { x: 1 }, `strict=${strict}`);
    });
  });

  test('close(true) with a live statement throws; finalize then close(true) succeeds', () => {
    withFileModes((db, strict) => {
      db.exec(SCHEMA);
      const live = db.prepare('SELECT 1 AS x');
      const locked = capture(() => db.close(true));
      assert.equal(locked.threw, true, `strict=${strict}`);
      if (!locked.threw) return;
      assert.equal(locked.message, 'database is locked', `strict=${strict}`);
      live.finalize();
      db.close(true);
    });
  });

  test('bun DatabaseOptions.strict is not SQLite CREATE TABLE STRICT', () => {
    withModes((db, strict) => {
      db.exec('CREATE TABLE ordinary (n INTEGER)');
      db.prepare('INSERT INTO ordinary (n) VALUES (?)').run('not-int');
      const stored = qprepare<IntRow>(db, 'SELECT n, typeof(n) AS t FROM ordinary').get();
      assert.equal(stored?.n, 'not-int', `strict=${strict}`);
      assert.equal(stored?.t, 'text', `strict=${strict}`);

      db.exec('CREATE TABLE us (n INTEGER) STRICT');
      const refused = capture(() => db.prepare('INSERT INTO us (n) VALUES (?)').run('not-int'));
      assert.equal(refused.threw, true, `strict=${strict}`);
      if (!refused.threw) return;
      assert.equal(refused.name, 'SQLiteError', `strict=${strict}`);
      assert.equal(
        refused.message,
        'cannot store TEXT value in INTEGER column us.n',
        `strict=${strict}`,
      );
    });
  });

  test('corpus SQL is positional ? only — named binds would be the breaking style', () => {
    const files = ['src/daemon/statements.ts', 'src/daemon/questions.ts', 'src/daemon/db.ts'];
    const named: { file: string; sql: string; binds: string[] }[] = [];
    let statementKeys = 0;
    for (const rel of files) {
      const source = readFileSync(path.join(REPO_ROOT, rel), 'utf8');
      if (rel.endsWith('statements.ts')) {
        statementKeys = (source.match(/^\s{4}(\w+):\s*db\.prepare/gm) ?? []).length;
      }
      for (const sql of preparedSql(source)) {
        const binds = namedPlaceholders(sql);
        if (binds.length) named.push({ file: rel, sql, binds });
      }
    }
    assert.equal(statementKeys, 112);
    assert.deepEqual(named, []);
    const statements = readFileSync(path.join(REPO_ROOT, 'src/daemon/statements.ts'), 'utf8');
    assert.match(statements, /stmt\.run\(\.\.\.keys\.map\(\(k\) => upd\[k\] \?\? null\), sid\);/);
  });
});
