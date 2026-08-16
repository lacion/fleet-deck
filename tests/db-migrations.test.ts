// tests/db-migrations.test.ts — the versioned-migration proof (R5/Step 3).
//
// db.ts moved from an additive, UNVERSIONED migrate() (probe with table_info,
// CREATE ... IF NOT EXISTS / conditional ALTER, no user_version, no transaction)
// to a numbered PRAGMA user_version ladder, each migration wrapped in its own
// transaction. This file pins the three invariants that make that change safe on
// a data-loss surface:
//
//   1. fresh → latest        — an empty DB migrates to the full schema + latest
//                              user_version.
//   2. existing v0 → converge — a DB written by the OLD code (full schema OR an
//                              early reduced shape, user_version still 0, rows
//                              present) converges to the SAME schema and latest
//                              user_version with ZERO data loss. This is the trap:
//                              fresh and populated DBs BOTH report user_version 0.
//   3. crash → rollback       — a migration that throws partway rolls back (schema
//                              AND version bump) and a clean re-run reaches latest.

import assert from 'node:assert/strict';
import {
  LATEST_USER_VERSION,
  type Migration,
  MIGRATIONS,
  migrate,
  openDb,
  readUserVersion,
} from '../src/daemon/db.ts';
import { openDatabase, type SqliteHandle } from '../src/daemon/sqlite.ts';
import test from './helpers/harness-test.ts';

// --- helpers ---------------------------------------------------------------

function tableExists(db: SqliteHandle, name: string): boolean {
  return !!db
    .prepare<{ name: string }>("SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?")
    .get(name);
}

function columnsOf(db: SqliteHandle, table: string): string[] {
  return db
    .prepare<{ name: string }>(`PRAGMA table_info(${table})`)
    .all()
    .map((r) => r.name)
    .sort();
}

// A structural fingerprint of a DB: every user table → its sorted column set,
// plus the sorted list of user indexes. Compares SHAPE independent of how each
// table's CREATE text was authored — a fresh `CREATE TABLE (all cols)` and an
// original CREATE + later `ALTER TABLE ADD COLUMN` yield the SAME columns but
// different stored sql, so column-set equality is the honest schema-equivalence
// check. (`sqlite_%` internal objects — autoindexes, sqlite_sequence — excluded.)
function schemaFingerprint(db: SqliteHandle): string {
  const objs = db
    .prepare<{ type: string; name: string }>(
      "SELECT type, name FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' AND type IN ('table', 'index') ORDER BY type, name",
    )
    .all();
  const shape: Record<string, string[]> = {};
  for (const o of objs) {
    if (o.type === 'table') shape[o.name] = columnsOf(db, o.name);
  }
  const indexes = objs
    .filter((o) => o.type === 'index')
    .map((o) => o.name)
    .sort();
  return JSON.stringify({ tables: shape, indexes });
}

// --- 1. fresh → latest -----------------------------------------------------

test('a fresh DB migrates to the full schema and the latest user_version', (t) => {
  const db = openDb(':memory:');
  t.after(() => db.close());

  assert.ok(LATEST_USER_VERSION >= 1, 'the ladder must define at least one version');
  assert.equal(readUserVersion(db), LATEST_USER_VERSION, 'a fresh DB reaches the latest version');

  for (const tbl of [
    'sessions',
    'file_touches',
    'mail',
    'events',
    'ticker',
    'conflicts',
    'commands',
    'questions',
    'spawns',
    'repos',
    'settings',
    'plans',
    'session_aliases',
  ]) {
    assert.ok(tableExists(db, tbl), `table ${tbl} must exist after a fresh migrate`);
  }

  // Representative late-added columns from the additive half are present.
  assert.ok(columnsOf(db, 'sessions').includes('run_id'), 'sessions.run_id must exist');
  assert.ok(columnsOf(db, 'spawns').includes('fail_detail'), 'spawns.fail_detail must exist');
  assert.ok(columnsOf(db, 'mail').includes('claimed_at'), 'mail.claimed_at must exist');
});

// --- 2. existing v0 (full current schema) → converge, no data loss ---------

test('an existing v0 DB with the full schema converges losslessly to the latest version', (t) => {
  // Reproduce a DB written by the OLD unversioned code: the full current schema,
  // but user_version never stamped. migration #1's up() IS that exact
  // (SCHEMA + additive backfill) sequence, so running it directly builds the
  // full schema and leaves user_version at 0 — precisely the old openDb output.
  const db = openDatabase(':memory:');
  t.after(() => db.close());
  const first = MIGRATIONS[0];
  assert.ok(first, 'the ladder must have a first migration');
  first.up(db);
  assert.equal(readUserVersion(db), 0, 'sanity: the hand-built v0 DB reports user_version 0');

  // Seed representative rows across several real tables.
  db.exec(
    `INSERT INTO sessions (session_id, callsign, cwd, col, source)
     VALUES ('s1', 'falcon', '/work/a', 'idle', 'hooks'), ('s2', 'otter', '/work/b', 'queued', 'hooks')`,
  );
  db.exec("INSERT INTO mail (to_session, from_id, text, at) VALUES ('s1', 's2', 'hello', 111)");
  db.exec(
    `INSERT INTO spawns (spawn_id, session_id, callsign, requested_at, status)
     VALUES ('sp1', 's1', 'falcon', 222, 'live')`,
  );

  const before = schemaFingerprint(db);

  // The trap: run the REAL versioned migrate over the populated v0 DB. Because
  // user_version is still 0, this RE-RUNS migration #1 over live data.
  migrate(db);

  assert.equal(readUserVersion(db), LATEST_USER_VERSION, 'the v0 DB must reach the latest version');

  // Zero data loss: every seeded row survives, unchanged.
  const rows = db
    .prepare<{ session_id: string; callsign: string; cwd: string; col: string }>(
      'SELECT session_id, callsign, cwd, col FROM sessions ORDER BY session_id',
    )
    .all();
  assert.equal(rows.length, 2, 'both sessions rows survive');
  assert.equal(rows[0]?.session_id, 's1');
  assert.equal(rows[0]?.callsign, 'falcon');
  assert.equal(rows[0]?.cwd, '/work/a');
  assert.equal(rows[0]?.col, 'idle');
  assert.equal(rows[1]?.session_id, 's2');
  assert.equal(rows[1]?.callsign, 'otter');
  assert.equal(rows[1]?.cwd, '/work/b');
  assert.equal(rows[1]?.col, 'queued');
  assert.equal(
    db.prepare<{ text: string }>("SELECT text FROM mail WHERE to_session = 's1'").get()?.text,
    'hello',
  );
  assert.equal(
    db.prepare<{ status: string }>("SELECT status FROM spawns WHERE spawn_id = 'sp1'").get()
      ?.status,
    'live',
  );

  // The re-run was a pure schema no-op, and the converged shape matches a fresh DB.
  assert.equal(schemaFingerprint(db), before, 'convergence must not alter the schema shape');
  const fresh = openDb(':memory:');
  t.after(() => fresh.close());
  assert.equal(
    schemaFingerprint(db),
    schemaFingerprint(fresh),
    'the converged v0 schema must equal a freshly-migrated schema',
  );
});

// --- 2b. existing v0 (reduced legacy shape) → additive ALTER, no data loss --

test('a reduced legacy sessions table gains its late columns additively with zero data loss', (t) => {
  // The more dangerous case, built WITHOUT the SCHEMA constant: a real early
  // fleetd shape (the 0.5.x sessions DDL — base columns through archived_at, but
  // none of the ticket/adopt/clear columns) with a live row. migrate() must
  // ALTER-ADD the missing columns over existing data without dropping a thing.
  const db = openDatabase(':memory:');
  t.after(() => db.close());
  db.exec(`CREATE TABLE sessions (
    session_id        TEXT PRIMARY KEY,
    callsign          TEXT,
    model             TEXT,
    cwd               TEXT,
    repo_id           TEXT,
    repo_name         TEXT,
    branch            TEXT,
    worktree          TEXT,
    col               TEXT DEFAULT 'queued',
    note              TEXT,
    task              TEXT,
    last_tool         TEXT,
    events            INTEGER DEFAULT 0,
    started_at        INTEGER,
    last_seen         INTEGER,
    ended_at          INTEGER,
    blocked_this_turn INTEGER DEFAULT 0,
    source            TEXT DEFAULT 'hooks',
    notification_type TEXT,
    archived_at       INTEGER
  )`);
  db.exec(
    `INSERT INTO sessions (session_id, callsign, cwd, col, note, events, source)
     VALUES ('old1', 'falcon', '/legacy', 'idle', 'kept', 7, 'hooks')`,
  );
  assert.equal(readUserVersion(db), 0, 'sanity: a hand-built table has user_version 0');
  assert.ok(!columnsOf(db, 'sessions').includes('ticket'), 'sanity: the legacy table lacks ticket');

  migrate(db);

  assert.equal(
    readUserVersion(db),
    LATEST_USER_VERSION,
    'the legacy DB must reach the latest version',
  );

  // Late columns were ALTER-added onto the existing table.
  for (const c of ['ticket', 'adopt_armed_until', 'end_reason', 'custom_suffix', 'run_id']) {
    assert.ok(
      columnsOf(db, 'sessions').includes(c),
      `column ${c} must be added to the legacy table`,
    );
  }

  // The pre-existing row is intact; the new columns backfill to NULL.
  const row = db
    .prepare<{
      callsign: string;
      cwd: string;
      note: string;
      events: number;
      ticket: string | null;
      run_id: string | null;
    }>('SELECT callsign, cwd, note, events, ticket, run_id FROM sessions WHERE session_id = ?')
    .get('old1');
  assert.ok(row, 'the legacy row must survive the migration');
  assert.equal(row.callsign, 'falcon');
  assert.equal(row.cwd, '/legacy');
  assert.equal(row.note, 'kept');
  assert.equal(Number(row.events), 7);
  assert.equal(row.ticket, null, 'ticket backfills to NULL');
  assert.equal(row.run_id, null, 'run_id backfills to NULL');

  // The BUG-107 alias backfill ran over the real row.
  assert.equal(
    db
      .prepare<{ callsign: string }>(
        "SELECT callsign FROM session_aliases WHERE session_id = 'old1'",
      )
      .get()?.callsign,
    'falcon',
  );

  // And the whole store now matches a freshly-migrated DB.
  const fresh = openDb(':memory:');
  t.after(() => fresh.close());
  assert.equal(
    schemaFingerprint(db),
    schemaFingerprint(fresh),
    'the migrated legacy schema must equal a freshly-migrated schema',
  );
});

// --- 3. crash → rollback → clean re-run ------------------------------------

test('a crash in the first migration rolls back fully; user_version stays 0; a clean re-run completes', (t) => {
  const db = openDatabase(':memory:');
  t.after(() => db.close());

  // A migration #1 that writes real state (a table + a row) and THEN throws.
  const boom: Migration = {
    version: 1,
    up(d) {
      d.exec('CREATE TABLE sessions (session_id TEXT PRIMARY KEY, cwd TEXT)');
      d.exec("INSERT INTO sessions (session_id, cwd) VALUES ('partial', '/x')");
      throw new Error('boom mid-migration');
    },
  };
  assert.throws(() => migrate(db, [boom]), /boom mid-migration/);

  assert.equal(readUserVersion(db), 0, 'a rolled-back migration must not advance user_version');
  assert.ok(
    !tableExists(db, 'sessions'),
    'the partial CREATE/INSERT must have rolled back (DDL is transactional in SQLite)',
  );

  // A clean re-run with the REAL ladder now completes to the latest version.
  migrate(db);
  assert.equal(
    readUserVersion(db),
    LATEST_USER_VERSION,
    'the clean re-run reaches the latest version',
  );
  assert.ok(tableExists(db, 'sessions'), 'the real schema is built by the clean re-run');
  assert.ok(tableExists(db, 'spawns'));
});

test('a crash in a LATER migration keeps committed migrations and rolls back only the failed one', (t) => {
  const db = openDatabase(':memory:');
  t.after(() => db.close());

  migrate(db); // to LATEST via the real ladder
  const committed = readUserVersion(db);
  assert.equal(committed, LATEST_USER_VERSION);

  const boom: Migration = {
    version: committed + 1,
    up(d) {
      d.exec('CREATE TABLE canary (x INTEGER)');
      d.exec('INSERT INTO canary (x) VALUES (1)');
      throw new Error(`kaboom in v${committed + 1}`);
    },
  };
  assert.throws(() => migrate(db, [...MIGRATIONS, boom]), /kaboom/);

  assert.equal(
    readUserVersion(db),
    committed,
    'the already-committed version must be intact after a later crash',
  );
  assert.ok(!tableExists(db, 'canary'), 'the failed migration rolled back fully');
  assert.ok(
    tableExists(db, 'sessions'),
    'migration #1 tables are never touched by the later crash',
  );

  // Fix the migration and re-run: only the pending one applies (#1 is skipped).
  const fixed: Migration = {
    version: committed + 1,
    up(d) {
      d.exec('CREATE TABLE canary (x INTEGER)');
      d.exec('INSERT INTO canary (x) VALUES (1)');
    },
  };
  migrate(db, [...MIGRATIONS, fixed]);
  assert.equal(
    readUserVersion(db),
    committed + 1,
    'the fixed re-run advances to the pending version',
  );
  assert.ok(tableExists(db, 'canary'));
  assert.equal(Number(db.prepare<{ x: number }>('SELECT x FROM canary').get()?.x), 1);
});

// --- idempotency: re-migrating an up-to-date DB is a no-op -----------------

test('migrate is idempotent: a second run on an up-to-date DB touches neither data nor version', (t) => {
  const db = openDb(':memory:');
  t.after(() => db.close());
  const v = readUserVersion(db);
  db.exec("INSERT INTO sessions (session_id, cwd) VALUES ('keep', '/k')");

  migrate(db);

  assert.equal(readUserVersion(db), v, 'the version is unchanged on a no-op re-run');
  assert.equal(
    db.prepare<{ cwd: string }>("SELECT cwd FROM sessions WHERE session_id = 'keep'").get()?.cwd,
    '/k',
    'existing data is untouched by a no-op re-run',
  );
});

// --- ladder-authoring guards: reject malformed migration sets --------------

test('migrate rejects a version outside the signed-32-bit user_version range', (t) => {
  const db = openDatabase(':memory:');
  t.after(() => db.close());

  // user_version is a signed 32-bit int; a larger value silently truncates on
  // write, so the runner must refuse it (interpolated straight into the PRAGMA).
  assert.throws(
    () => migrate(db, [{ version: 0x80000000, up() {} }]),
    /invalid migration version/,
    'a version > 0x7fffffff is rejected',
  );
  assert.throws(
    () => migrate(db, [{ version: 0, up() {} }]),
    /invalid migration version/,
    'version 0 is rejected (0 is the fresh-DB sentinel)',
  );
  assert.throws(
    () => migrate(db, [{ version: 1.5, up() {} }]),
    /invalid migration version/,
    'a non-integer version is rejected',
  );
  // A rejected set must not have advanced anything.
  assert.equal(readUserVersion(db), 0, 'a rejected ladder leaves user_version untouched');
});

test('migrate rejects a ladder with a duplicate version', (t) => {
  const db = openDatabase(':memory:');
  t.after(() => db.close());

  // Two migrations claiming the same version: once current passes it, the second
  // up() silently never runs — an authoring bug, so the runner must fail loud.
  assert.throws(
    () =>
      migrate(db, [
        { version: 1, up() {} },
        { version: 1, up() {} },
      ]),
    /duplicate migration version/,
  );
});
