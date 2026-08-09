// tests/settings-transaction.test.mjs
//
// BUG-047 — one settings POST must be ONE transaction.
//
// The defect: setSettings validated every key up front, then committed each key
// as an independent autocommit. A storage failure (SQLITE_BUSY, SQLITE_FULL,
// I/O) partway through left the EARLIER keys durable while the endpoint
// reported failure — and the caller believed nothing persisted. The concrete
// worst case was a body of {gateway_base_url, gateway_token} failing on the
// token write: the new host committed, so later spawns combined the NEW host
// with the OLD credential, disclosing a live gateway secret to an unintended
// endpoint.
//
// These tests exercise createSettings directly against an in-memory DB with a
// wrapped `q.setSetting` that throws mid-body — the deterministic stand-in for
// the storage failures above.

import test from 'node:test';
import assert from 'node:assert/strict';
import { openDb } from '../scripts/fleetd/db.ts';
import { createStatements } from '../scripts/fleetd/statements.ts';
import { createSettings } from '../scripts/fleetd/settings.mjs';

/** A createSettings wired to a real in-memory DB, with hooks to make a
 * chosen settings key's write fail and to observe mutation broadcasts. */
function settingsHarness({ failOnKey = null } = {}) {
  const db = openDb(':memory:');
  const { q } = createStatements(db);
  const calls = [];
  const realRun = q.setSetting.run.bind(q.setSetting);
  q.setSetting = {
    ...q.setSetting,
    run: (key, value, now) => {
      calls.push(key);
      if (key === failOnKey) throw new Error('SQLITE_BUSY: database is locked');
      return realRun(key, value, now);
    },
  };
  let mutations = 0;
  const settings = createSettings({
    db, q,
    onMutate: () => { mutations += 1; },
    resolveReposDir: () => ({ value: '/tmp', source: 'default' }),
    setReposDir: value => q.setSetting.run('repos_dir', value ?? null, Date.now()),
    resolveRepoDefaultOrg: () => ({ value: null, source: 'default' }),
    validateRepoDefaultOrg: v => v,
  });
  const read = key => q.getSetting.get(key)?.value ?? null;
  return { db, settings, calls, read, mutations: () => mutations };
}

test('settings transaction: a storage failure mid-body rolls back EVERY key and reports 5xx', () => {
  const { db, settings, calls, read, mutations } = settingsHarness({ failOnKey: 'gateway_token' });
  try {
    const out = settings.setSettings({
      gateway_base_url: 'https://gw.example.com',
      gateway_token: 'super-secret',
    });
    assert.ok(out.status >= 500 && out.status < 600,
      `a storage failure is not the caller's mistake — expected 5xx, got ${out.status}`);
    assert.equal(out.body.ok, false);
    assert.deepEqual(calls, ['gateway_base_url', 'gateway_token'],
      'sanity: both commits were attempted before the failure');
    assert.equal(read('gateway_base_url'), null,
      'the earlier key must roll back too — nothing in a failed request may persist');
    assert.equal(read('gateway_token'), null);
    assert.equal(mutations(), 0,
      'onMutate must not broadcast a /state frame for a request that committed nothing');
  } finally {
    db.close();
  }
});

test('settings transaction: a rollback leaves the previously stored credential intact', () => {
  // The BUG-047 disclosure scenario: a good profile is stored, then a later
  // update to base_url + token fails on the token write. Before the fix the
  // new host survived and paired with the old credential; after it, the store
  // must be EXACTLY what it was before the rejected request.
  const { db, settings, read } = settingsHarness({ failOnKey: 'gateway_token' });
  try {
    db.prepare('INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)')
      .run('gateway_base_url', 'https://gw-old.example.com', Date.now());
    db.prepare('INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)')
      .run('gateway_token', 'old-secret', Date.now());

    const out = settings.setSettings({
      gateway_base_url: 'https://attacker.example.com',
      gateway_token: 'new-secret',
    });
    assert.ok(out.status >= 500, `expected 5xx, got ${out.status}`);
    assert.equal(read('gateway_base_url'), 'https://gw-old.example.com',
      'a rejected request must not move the host the stored credential talks to');
    assert.equal(read('gateway_token'), 'old-secret');
  } finally {
    db.close();
  }
});

test('settings transaction: a clean multi-key body still commits atomically and broadcasts once', () => {
  const { db, settings, read, mutations } = settingsHarness();
  try {
    const out = settings.setSettings({
      gateway_base_url: 'https://gw.example.com',
      gateway_token: 'super-secret',
      gateway_auth_style: 'api-key',
    });
    assert.equal(out.status, 200, out.body.reason);
    assert.equal(read('gateway_base_url'), 'https://gw.example.com');
    assert.equal(read('gateway_token'), 'super-secret');
    assert.equal(read('gateway_auth_style'), 'api-key');
    assert.equal(out.body.settings.gateway.ready, true);
    assert.equal(mutations(), 1);
  } finally {
    db.close();
  }
});

test('settings transaction: a validation error is still a 400 and still writes nothing', () => {
  const { db, settings, calls, read, mutations } = settingsHarness();
  try {
    const out = settings.setSettings({
      gateway_base_url: 'https://gw.example.com',
      gateway_auth_style: 'nonsense',
    });
    assert.equal(out.status, 400, 'validator rejections keep their tagged status');
    assert.deepEqual(calls, [], 'prepare-all-before-commit: no write was even attempted');
    assert.equal(read('gateway_base_url'), null);
    assert.equal(mutations(), 0);
  } finally {
    db.close();
  }
});
