// tests/loopback-gates.test.mjs
//
// 0.16.0 — token provisioning + the gated loopback powers. The daemon now
// mints $FLEETDECK_HOME/token on EVERY boot (default loopback included), and
// the powers a malicious local process / a fleet agent must not wield
// anonymously require it even with FLEETDECK_REQUIRE_TOKEN off: /ws/term,
// POST /mail, gateway_* settings writes, and the unsupervised arm. Ordinary
// loopback routes stay open. 0.16.1 adds an explicit plain-loopback opt-out for
// those four powers without weakening hook authentication.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, existsSync, mkdtempSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import WebSocket from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.mjs';
import { postJson, getJson } from './helpers/http.mjs';

function scratchHome() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-loopback-gates-'));
}

function wsTermAttempt(url) {
  return new Promise((resolve) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => { ws.terminate(); resolve('timeout'); }, 3000);
    ws.on('open', () => { clearTimeout(timer); ws.close(); resolve('opened'); });
    ws.on('error', () => { clearTimeout(timer); resolve('refused'); });
  });
}

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

  const bare = await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`);
  assert.equal(bare, 'refused', 'tokenless /ws/term must be refused at upgrade');

  const authed = await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever&t=${daemon.token}`);
  // The upgrade itself must succeed; the spawn id is bogus so the bridge will
  // close us right after — 'opened' is the assertion, not a long-lived socket.
  assert.equal(authed, 'opened', 'bearer /ws/term passes the upgrade gate');
});

test('TRUST_LOOPBACK=on waives the four power gates only for plain loopback', async (t) => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'on' } });
  t.after(() => daemon.stop());

  const term = await wsTermAttempt(`ws://127.0.0.1:${daemon.port}/ws/term?spawn=whatever`);
  assert.equal(term, 'opened', 'tokenless /ws/term upgrade opens');

  const mail = await postJson(`${daemon.baseUrl}/mail`, { to: 'all', from: 'operator', text: 'trusted loopback' });
  assert.equal(mail.status, 200, `tokenless POST /mail opens: ${mail.text}`);

  const gateway = await postJson(`${daemon.baseUrl}/api/settings`, { gateway_base_url: 'https://gateway.example.com' });
  assert.equal(gateway.status, 200, `tokenless gateway_* write opens: ${gateway.text}`);

  const arm = await postJson(`${daemon.baseUrl}/api/spawn/arm-unsupervised`, {});
  assert.equal(arm.status, 200, `tokenless unsupervised arm opens: ${arm.text}`);
  assert.ok(typeof arm.json?.arm_token === 'string' && arm.json.arm_token, 'arm capability is minted');

  // Hook authentication cannot be opted out. Legacy tokenless hooks retain
  // their fail-open HTTP dialect, but are refused before any handler runs.
  const hook = await postJson(`${daemon.baseUrl}/hook/SessionStart`, { session_id: 'trust-loopback-forgery' });
  assert.equal(hook.status, 200, 'legacy hook refusal stays fail-open at HTTP level');
  assert.equal(hook.json?.ok, undefined, 'tokenless hook was not authenticated');
  assert.match(hook.json?.hookSpecificOutput?.additionalContext ?? '', /restart/i);
});

test('TRUST_LOOPBACK refuses contradictory and invalid startup configuration', async (t) => {
  const cases = [
    {
      name: 'REQUIRE_TOKEN=on',
      env: { FLEETDECK_TRUST_LOOPBACK: 'on', FLEETDECK_REQUIRE_TOKEN: 'on' },
      message: /FLEETDECK_TRUST_LOOPBACK=on conflicts with FLEETDECK_REQUIRE_TOKEN=on/,
    },
    {
      name: 'LAN bind',
      env: { FLEETDECK_TRUST_LOOPBACK: 'on', FLEETDECK_BIND: '0.0.0.0' },
      message: /FLEETDECK_TRUST_LOOPBACK=on requires a loopback FLEETDECK_BIND/,
    },
    {
      name: 'invalid value',
      env: { FLEETDECK_TRUST_LOOPBACK: 'sometimes' },
      message: /FLEETDECK_TRUST_LOOPBACK must be 'on' or 'off'/,
    },
  ];

  for (const entry of cases) {
    await t.test(entry.name, async () => {
      const home = scratchHome();
      const raw = spawnRaw({ port: randomPort(), home, env: entry.env });
      try {
        const code = await raw.waitForExit();
        assert.notEqual(code, 0, 'contradictory configuration must not start');
        assert.match(raw.stderr, entry.message);
      } finally {
        await raw.kill();
        rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      }
    });
  }
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
