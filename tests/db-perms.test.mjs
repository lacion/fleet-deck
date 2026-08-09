// tests/db-perms.test.mjs — regression coverage for BUG #6 (world-readable
// state). The July audit hardened the token file and fleetd.log to 0600 but
// missed the SQLite store, which holds session cwds, callsigns, mail, commands,
// plan text and raw permission/question payloads. openDb must pin fleetd.db —
// and its WAL/SHM sidecars — owner-only. Pure: opens a DB directly, no daemon.

import test from 'node:test';
import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { openDb } from '../scripts/fleetd/db.ts';

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

// BUG-110: an unestablishable owner-only mode must not silently degrade to a
// world-readable state DB. The audit trigger is a chmod that fails (EPERM on a
// shared HOME not owned by the daemon UID) or a filesystem whose mode bits do
// not hold — either way SQLite opens and updates the file while the final
// stat still shows group/other access. A real EPERM needs a foreign-owned
// file, which an unprivileged test cannot construct (chown needs root), so
// the fs seam simulates the refusal on a REAL database file.
test('openDb refuses to hand back a DB whose owner-only mode cannot be established', (t) => {
  const home = scratch(t);
  const dbFile = path.join(home, 'fleetd.db');

  // Case 1: chmod throws (EPERM on a shared HOME / chmod-refusing filesystem).
  const eperm = Object.assign(new Error('operation not permitted'), { code: 'EPERM' });
  assert.throws(
    () => openDb(dbFile, { chmodSync: () => { throw eperm; }, statSync }),
    /owner-only confidentiality could not be established/,
    'a chmod refusal on the main DB file must abort startup, not silently serve a permissive DB',
  );

  // Case 2: chmod "succeeds" but the mode never tightens (filesystem without
  // real mode bits) — the post-chmod stat verification must catch it.
  assert.throws(
    () => openDb(dbFile, {
      chmodSync: () => {},
      statSync: (p) => Object.assign(statSync(p), { mode: 0o100644 }),
    }),
    /owner-only confidentiality could not be established/,
    'a post-chmod stat that still shows group/other access must abort startup',
  );

  // Case 3: the refusal must not fire for a lazily ABSENT WAL sidecar (ENOENT
  // is expected at open time), only for a sidecar that exists but cannot be
  // pinned owner-only.
  const enoentOnWal = (p, m) => {
    if (p.endsWith('-wal')) throw Object.assign(new Error('no such file or directory'), { code: 'ENOENT' });
    return chmodSync(p, m);
  };
  const db = openDb(dbFile, { chmodSync: enoentOnWal, statSync });
  t.after(() => { try { db.close(); } catch { /* already closed */ } });
  assert.ok(db, 'a lazily absent WAL sidecar (ENOENT) must not trip the refusal');

  assert.throws(
    () => openDb(dbFile, {
      chmodSync: (p, m) => {
        if (p.endsWith('-shm')) throw eperm;
        return chmodSync(p, m);
      },
      statSync,
    }),
    /sidecar owner-only confidentiality could not be established/,
    'an existing sidecar whose chmod fails must also abort startup',
  );
});

// The mirror-image contract: the refusal must ONLY fire when confidentiality
// genuinely cannot be established. A pre-existing owner-only DB, and lazily
// absent WAL/SHM sidecars (ENOENT), must open exactly as before.
test('openDb still opens a pre-existing DB and tolerates lazily absent sidecars', (t) => {
  const home = scratch(t);
  const dbFile = path.join(home, 'fleetd.db');
  // First open creates everything; close checkpoints and removes the sidecars,
  // so the SECOND open is the ENOENT-sidecar path the fix must not break.
  const first = openDb(dbFile);
  first.exec("INSERT INTO sessions (session_id, cwd) VALUES ('preexisting', '/x')");
  first.close();

  const db = openDb(dbFile);
  t.after(() => { try { db.close(); } catch { /* already closed */ } });
  assert.equal(
    db.prepare('SELECT cwd FROM sessions WHERE session_id = ?').get('preexisting')?.cwd,
    '/x',
    'a re-opened owner-only DB with absent sidecars must still work',
  );
  if (MODE_BITS_MEANINGFUL) {
    assert.equal(statSync(dbFile).mode & 0o077, 0, 're-opened DB must stay owner-only');
  }
});
