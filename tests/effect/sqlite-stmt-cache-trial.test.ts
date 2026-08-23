// sqlite-stmt-cache-trial.test.ts — P8.5 trial of bun:sqlite Database.query()
// caching at Bun 1.3.14 against the daemon statement corpus and the P8.3
// finalize-then-close(true) invariant.
//
// Fixtures open scratch DBs only (`:memory:` or mkdtemp); they never touch the
// daemon's store. This file does NOT add query() to SqliteHandle and does NOT
// change any daemon callsite. See docs/v1/evidence/effect/p8-stmt-cache-trial.md.

import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { Database } from 'bun:sqlite';
import { describe, test } from 'bun:test';
import { openDatabase } from '../../src/daemon/sqlite.ts';

const REPO_ROOT = path.resolve(import.meta.dir, '../..');
const Q_MODULES = [
  'commands',
  'derive',
  'events',
  'files',
  'ingest',
  'ledger',
  'mail',
  'plans',
  'questions',
  'repos',
  'retention',
  'settings',
  'snapshot',
  'spawns',
  'worktrees',
] as const;

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

function withFile(fn: (db: Database) => void): void {
  const dir = mkdtempSync(path.join(tmpdir(), 'fd-p8-stmtcache-'));
  const db = new Database(path.join(dir, 'trial.db'));
  try {
    fn(db);
  } finally {
    try {
      db.close();
    } catch {
      // close(true) may already have closed or refused; always drop the scratch dir.
    }
    rmSync(dir, { recursive: true, force: true });
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

function queryCacheSize(): number {
  return (Database as unknown as { MAX_QUERY_CACHE_SIZE: number }).MAX_QUERY_CACHE_SIZE;
}

describe('P8.5 bun:sqlite Database.query() cache trial (Bun 1.3.14)', () => {
  test('the daemon seam still has no query() and wrap tracks only prepare()', () => {
    const source = readFileSync(path.join(REPO_ROOT, 'src/daemon/sqlite.ts'), 'utf8');
    assert.match(source, /prepare<R = SqlRow>\(sql: string\): SqliteStatement<R>;/);
    assert.doesNotMatch(source, /query\s*<R/);
    assert.doesNotMatch(source, /handle\.query\(/);
    assert.match(source, /const stmt = handle\.prepare\(sql\);/);
    assert.match(source, /statements\.add\(stmt\);/);
    assert.match(source, /finalizeStatements\(\) \{/);

    const h = openDatabase(':memory:');
    assert.equal(typeof (h as { query?: unknown }).query, 'undefined');
    h.close();
  });

  test('corpus prepares are compile-once q + two questions ephemerals + four boot pragmas', () => {
    const statements = readFileSync(path.join(REPO_ROOT, 'src/daemon/statements.ts'), 'utf8');
    const questions = readFileSync(path.join(REPO_ROOT, 'src/daemon/questions.ts'), 'utf8');
    const dbSrc = readFileSync(path.join(REPO_ROOT, 'src/daemon/db.ts'), 'utf8');

    const qKeys = statements.match(/^\s{4}(\w+):\s*db\.prepare/gm) ?? [];
    assert.equal(qKeys.length, 112);

    const statementSql = preparedSql(statements);
    // 112 q keys + the updateSession shape template. The comment on line 11 also
    // contains the characters `db.prepare` but is not a callsite.
    assert.equal(statementSql.length, 113);
    const qSql = statementSql.slice(0, 112);
    assert.equal(new Set(qSql).size, 112, 'q map SQL is unique — no query() dedup payoff');

    assert.equal((questions.match(/db\.prepare(?:<[^>]*>)?\(/g) ?? []).length, 9);
    assert.match(questions, /rearmPending\.finalize\(\);/);
    assert.match(questions, /purge\.finalize\(\);/);

    assert.equal((dbSrc.match(/\.prepare(?:<[^>]*>)?\(/g) ?? []).length, 4);
    assert.match(dbSrc, /PRAGMA table_info\(sessions\)/);
    assert.match(dbSrc, /PRAGMA table_info\(mail\)/);
    assert.match(dbSrc, /PRAGMA table_info\(spawns\)/);
    assert.match(dbSrc, /PRAGMA user_version/);

    assert.match(statements, /const updateStmts = new Map<string, SqliteStatement>/);

    let qCalls = 0;
    for (const mod of Q_MODULES) {
      const src = readFileSync(path.join(REPO_ROOT, `src/daemon/${mod}.ts`), 'utf8');
      qCalls += (src.match(/\bq\.[A-Za-z0-9_]+\.(?:run|get|all)\(/g) ?? []).length;
    }
    assert.equal(qCalls, 303);

    for (const rel of [
      'src/daemon/sqlite.ts',
      'src/daemon/statements.ts',
      'src/daemon/questions.ts',
      'src/daemon/db.ts',
    ]) {
      const src = readFileSync(path.join(REPO_ROOT, rel), 'utf8');
      assert.doesNotMatch(src, /\bdb\.query\s*\(/);
    }
  });

  test('MAX_QUERY_CACHE_SIZE defaults to 20 (writable static, not in bun-types)', () => {
    assert.equal(queryCacheSize(), 20);
  });

  test('query() caches by exact SQL string; prepare() is always a new object', () => {
    const db = new Database(':memory:');
    const sql = 'SELECT 1 AS n';
    const q1 = db.query(sql);
    const q2 = db.query(sql);
    const interned = db.query('SELECT ' + '1 AS n');
    const ws = db.query('SELECT 1 AS n ');
    const nl = db.query('SELECT 1 AS n\n');
    const cased = db.query('select 1 AS n');
    const commented = db.query('SELECT 1 AS n -- x');
    const p1 = db.prepare(sql);
    const p2 = db.prepare(sql);

    assert.equal(q1, q2);
    assert.equal(q1, interned);
    assert.notEqual(q1, ws);
    assert.notEqual(q1, nl);
    assert.notEqual(q1, cased);
    assert.notEqual(q1, commented);
    assert.notEqual(p1, p2);
    assert.notEqual(q1, p1);
    db.close();
  });

  test('cache is first-20-win, not LRU: slots 0..19 hit, 20+ miss', () => {
    const db = new Database(':memory:');
    const held: object[] = [];
    for (let i = 0; i < 40; i++) held.push(db.query(`SELECT ${i} AS n`));

    assert.equal(db.query('SELECT 0 AS n'), held[0], 'oldest of 40 still cached');
    assert.equal(db.query('SELECT 19 AS n'), held[19]);
    assert.notEqual(db.query('SELECT 20 AS n'), held[20], '21st SQL is not cached');
    assert.notEqual(db.query('SELECT 39 AS n'), held[39]);

    // Touching a late SQL does not evict slot 0 — not LRU on 1.3.14.
    db.query('SELECT 39 AS n');
    assert.equal(db.query('SELECT 0 AS n'), held[0]);
    db.close();
  });

  test('query() aliases share bind state; finalize one alias kills all', () => {
    const db = new Database(':memory:');
    db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)');
    db.exec("INSERT INTO t VALUES (1, 'a'), (2, 'b')");
    const sql = 'SELECT name FROM t WHERE id = ?';
    const a = db.query(sql);
    const b = db.query(sql);
    assert.equal(a, b);
    assert.deepEqual(a.get(1), { name: 'a' });
    assert.deepEqual(b.get(), { name: 'a' }, 'omit-after-bind is shared');
    a.finalize();
    const dead = capture(() => b.get(2));
    assert.equal(dead.threw, true);
    if (dead.threw) assert.equal(dead.message, 'Statement has finalized');
    const fresh = db.query(sql);
    assert.notEqual(fresh, a);
    assert.deepEqual(fresh.get(2), { name: 'b' });
    db.close();
  });

  test('SELECT * schema is frozen on a cached query until finalize + requery', () => {
    const db = new Database(':memory:');
    db.exec('CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT)');
    db.exec("INSERT INTO t (id, a) VALUES (1, 'x')");
    const sql = 'SELECT * FROM t WHERE id = ?';
    const q = db.query(sql);
    assert.deepEqual(q.get(1), { id: 1, a: 'x' });

    db.exec("ALTER TABLE t ADD COLUMN b TEXT DEFAULT 'y'");
    assert.equal(db.query(sql), q, 'requery hits the frozen cache');
    assert.deepEqual(q.get(1), { id: 1, a: 'x' });
    assert.equal(Object.hasOwn(q.get(1) as object, 'b'), false);

    q.finalize();
    const fresh = db.query(sql);
    assert.notEqual(fresh, q);
    assert.deepEqual(fresh.get(1), { id: 1, a: 'x', b: 'y' });
    db.close();
  });

  test('close(true) occupancy: ≤20 query() succeed (bun finalizes the cache); ≥21 lock', () => {
    for (const n of [1, 19, 20, 21, 32, 112]) {
      withFile((db) => {
        const stmts: Array<{ get: () => unknown; finalize: () => void }> = [];
        for (let i = 0; i < n; i++) stmts.push(db.query(`SELECT ${i} AS n`));
        for (const s of stmts) s.get();
        const result = capture(() => db.close(true));
        if (n <= 20) {
          assert.equal(result.threw, false, `n=${n} should close`);
          const after = capture(() => stmts[0]?.get());
          assert.equal(after.threw, true, `n=${n} cache finalized on close`);
          if (after.threw) assert.equal(after.message, 'Statement has finalized');
        } else {
          assert.equal(result.threw, true, `n=${n} should lock`);
          if (result.threw) assert.equal(result.message, 'database is locked');
        }
      });
    }
  });

  test('overflow: index 20 stays live; finalize it then close(true) recovers', () => {
    withFile((db) => {
      const stmts: Array<{ get: () => unknown; finalize: () => void }> = [];
      for (let i = 0; i < 21; i++) stmts.push(db.query(`SELECT ${i} AS n`));
      for (const s of stmts) s.get();
      const locked = capture(() => db.close(true));
      assert.equal(locked.threw, true);
      if (locked.threw) assert.equal(locked.message, 'database is locked');

      const live = stmts.map((s, i) => ({ i, live: !capture(() => s.get()).threw }));
      assert.equal(live.filter((x) => x.live).length, 1);
      assert.equal(live.find((x) => x.live)?.i, 20);

      stmts[20]?.finalize();
      db.close(true);
    });
  });

  test('dropping JS refs of 21 query() statements does not unlock close(true)', () => {
    withFile((db) => {
      for (let i = 0; i < 21; i++) db.query(`SELECT ${i} AS n`).get();
      const locked = capture(() => db.close(true));
      assert.equal(locked.threw, true);
      if (locked.threw) assert.equal(locked.message, 'database is locked');
    });
  });

  test('wrap-Set replica: 1 untracked query() after finalize(prepares) still close(true)s', () => {
    withFile((db) => {
      const tracked = new Set<{ finalize(): void }>();
      const prepare = (sql: string) => {
        const stmt = db.prepare(sql);
        tracked.add(stmt);
        return stmt;
      };
      prepare('SELECT 1 AS x').get();
      db.query('SELECT 2 AS x').get();
      for (const s of tracked) s.finalize();
      tracked.clear();
      db.close(true);
    });
  });

  test('wrap-Set replica: 21 untracked query() after finalize(prepares) lock close(true)', () => {
    withFile((db) => {
      const tracked = new Set<{ finalize(): void }>();
      const p = db.prepare('SELECT 1 AS x');
      tracked.add(p);
      p.get();
      for (let i = 0; i < 21; i++) db.query(`SELECT ${i} AS n`).get();
      for (const s of tracked) s.finalize();
      const locked = capture(() => db.close(true));
      assert.equal(locked.threw, true);
      if (locked.threw) assert.equal(locked.message, 'database is locked');
    });
  });

  test('openDatabase wrap: 112 prepares lock close(true) until finalizeStatements', () => {
    const h = openDatabase(':memory:');
    for (let i = 0; i < 112; i++) h.prepare(`SELECT ${i} AS n`).get();
    const before = capture(() => h.close(true));
    assert.equal(before.threw, true);
    if (before.threw) assert.equal(before.message, 'database is locked');
    h.finalizeStatements();
    h.close(true);
  });

  test('openDatabase wrap: four boot-style pragma prepares lock until finalizeStatements', () => {
    const h = openDatabase(':memory:');
    h.exec(`
      CREATE TABLE sessions (id INTEGER);
      CREATE TABLE mail (id INTEGER);
      CREATE TABLE spawns (id INTEGER);
    `);
    h.prepare('PRAGMA user_version').get();
    h.prepare('PRAGMA table_info(sessions)').all();
    h.prepare('PRAGMA table_info(mail)').all();
    h.prepare('PRAGMA table_info(spawns)').all();
    const before = capture(() => h.close(true));
    assert.equal(before.threw, true);
    if (before.threw) assert.equal(before.message, 'database is locked');
    h.finalizeStatements();
    h.close(true);
  });

  test('mix: one live prepare + one query, no finalize, locks close(true) because of prepare', () => {
    withFile((db) => {
      db.prepare('SELECT 1 AS x').get();
      db.query('SELECT 2 AS x').get();
      const locked = capture(() => db.close(true));
      assert.equal(locked.threw, true);
      if (locked.threw) assert.equal(locked.message, 'database is locked');
    });
  });
});
