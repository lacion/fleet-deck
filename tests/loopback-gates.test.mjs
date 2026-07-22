// tests/loopback-gates.test.mjs
//
// 0.16.0 — token provisioning + the gated loopback powers. The daemon now
// mints $FLEETDECK_HOME/token on EVERY boot (default loopback included), and
// the powers a malicious local process / a fleet agent must not wield
// anonymously require it even with FLEETDECK_REQUIRE_TOKEN off: /ws/term,
// POST /mail, gateway_* settings writes, and the unsupervised arm. Ordinary
// loopback routes stay open — the documented trust zone is unchanged.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync, statSync } from 'node:fs';
import path from 'node:path';
import WebSocket from 'ws';
import { startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson } from './helpers/http.mjs';

test('the token is minted 0600 on every boot, and the file matches the daemon', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const file = path.join(daemon.home, 'token');
  assert.ok(existsSync(file), 'token file exists in default loopback mode');
  const mode = statSync(file).mode & 0o777;
  assert.equal(mode, 0o600, 'token file is owner-only');
  assert.equal(readFileSync(file, 'utf8').trim(), daemon.token, 'handle surfaces the same token');
  assert.ok(daemon.token.length >= 32, 'token has real entropy');
});

test('gateway_* settings writes require the bearer; plain settings keys stay open', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  // gateway_* keys: 401 without a token.
  const bare = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_base_url: 'https://gateway.example.com' });
  assert.equal(bare.status, 401, 'tokenless gateway write must 401');

  // With the token: the normal validation path runs (and a valid value saves).
  const authed = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_base_url: 'https://gateway.example.com' }, { token: daemon.token });
  assert.equal(authed.status, 200, `authenticated gateway write succeeds: ${JSON.stringify(authed.json)}`);

  // A non-gateway key keeps the loopback exemption.
  const plain = await postJson(`${daemon.baseUrl}/api/settings`, { browse_root: daemon.home });
  assert.equal(plain.status, 200, 'browse_root still open on loopback');
});

test('/ws/term refuses a tokenless loopback client and accepts the bearer', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const bare = await new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`);
    ws.on('open', () => { ws.close(); resolve('opened'); });
    ws.on('error', () => resolve('refused'));
    setTimeout(() => resolve('timeout'), 3000);
  });
  assert.equal(bare, 'refused', 'tokenless /ws/term must be refused at upgrade');

  const authed = await new Promise((resolve) => {
    const ws = new WebSocket(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever&t=${daemon.token}`);
    ws.on('open', () => { ws.close(); resolve('opened'); });
    ws.on('error', () => resolve('refused'));
    setTimeout(() => resolve('timeout'), 3000);
  });
  // The upgrade itself must succeed; the spawn id is bogus so the bridge will
  // close us right after — 'opened' is the assertion, not a long-lived socket.
  assert.equal(authed, 'opened', 'bearer /ws/term passes the upgrade gate');
});

test('ordinary loopback routes keep the historical exemption', async (t) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  assert.equal((await getJson(`${daemon.baseUrl}/state`)).status, 200, '/state open');
  assert.equal((await getJson(`${daemon.baseUrl}/health`)).status, 200, '/health open');
  assert.equal((await getJson(`${daemon.baseUrl}/api/settings`)).status, 200, 'GET settings open');
  const cleanup = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(cleanup.status, 200, 'POST /api/cleanup open');
});
