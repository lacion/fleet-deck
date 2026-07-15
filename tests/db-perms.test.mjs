// tests/db-perms.test.mjs — regression coverage for BUG #6 (world-readable
// state). The July audit hardened the token file and fleetd.log to 0600 but
// missed the SQLite store, which holds session cwds, callsigns, mail, commands,
// plan text and raw permission/question payloads. openDb must pin fleetd.db —
// and its WAL/SHM sidecars — owner-only. Pure: opens a DB directly, no daemon.

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.mjs';

function scratch(t, prefix = 'fleetdeck-dbperms-') {
  const dir = mkdtempSync(path.join(tmpdir(), prefix));
  t.after(() => rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return dir;
}

// The load-bearing invariant is "no group/other access" (mode & 0o077 === 0);
// the exact 0o600 is what chmod sets on a POSIX host. On platforms where the
// filesystem cannot represent Unix mode bits (e.g. Windows) statSync reports a
// synthesized mode and the 0o077 check may not hold — hence the guard below.
const MODE_BITS_MEANINGFUL = process.platform !== 'win32';

test('openDb pins fleetd.db and its WAL/SHM sidecars to owner-only (0600)', (t) => {
  const home = scratch(t);
  const dbFile = path.join(home, 'fleetd.db');
  const db = openDb(dbFile);
  t.after(() => { try { db.close(); } catch { /* already closed */ } });

  // Force real WAL activity so the -wal/-shm sidecars exist while we stat them
  // (they are created lazily on first write, and a checkpoint at close can
  // delete them — so assert with the DB still open).
  db.exec("INSERT INTO sessions (session_id, cwd) VALUES ('perm-test', '/secret/cwd')");

  assert.equal(existsSync(dbFile), true, 'openDb must have created the DB file');
  if (!MODE_BITS_MEANINGFUL) {
    t.skip('POSIX mode bits are not meaningful on this platform');
    return;
  }

  assert.equal(statSync(dbFile).mode & 0o777, 0o600, 'fleetd.db must be owner-only (0600)');

  // Sidecars are best-effort at open time (lazily created) but WAL mode + the
  // INSERT above should have materialized them; assert the real invariant (no
  // group/other bits) on whichever are present.
  for (const sidecar of [`${dbFile}-wal`, `${dbFile}-shm`]) {
    if (!existsSync(sidecar)) continue;
    assert.equal(statSync(sidecar).mode & 0o077, 0, `${path.basename(sidecar)} must not be group/other-accessible`);
  }
});

test('openDb(":memory:") does not throw trying to chmod a pathless DB', (t) => {
  // Existing suites open in-memory DBs; the pathless chmod must be swallowed.
  const db = openDb(':memory:');
  t.after(() => { try { db.close(); } catch { /* already closed */ } });
  assert.ok(db, 'in-memory DB opens without a filesystem chmod throwing');
});
