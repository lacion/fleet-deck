// tests/require-token.test.mjs
//
// FLEETDECK_REQUIRE_TOKEN=on — opt into the token even on pure loopback. On a
// multi-user machine every other OS user can reach 127.0.0.1 and today inherits
// the loopback auto-authorize (tokenless /state, /api/spawn, the lot). With the
// flag on that exemption survives for exactly two callers: a /hook/* path
// (Claude Code http hooks cannot attach an Authorization header) and the
// data-free public shell (a browser must load it before it can present a key).
// Everything else — data routes, exec routes, both WebSockets — falls through to
// the bearer/?t= check even on loopback.
//
// The gate under test lives in http.mjs authorized(); it is additive, so the
// LAST test pins that with the flag OFF a loopback data route stays wide open
// (today's behavior), guaranteeing this never regresses the default.

import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocket } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.mjs';
import { waitForResponse, scaleMs } from './helpers/wait.mjs';
import { loadFixture } from './helpers/fixtures.mjs';

// >= 16 characters after trimming, or the daemon refuses to start.
const TOKEN = 'require-token-suite-0123456789abcdef';
const sleep = ms => new Promise(r => setTimeout(r, ms));

function scratchHome() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-require-token-'));
}

// Loopback daemon with FLEETDECK_REQUIRE_TOKEN=on and a KNOWN token, so the
// tests can present it deterministically. Loopback bind ⇒ the peer address is
// 127.0.0.1, exactly the case the flag is designed to gate. Health is itself
// gated under the flag, so we wait on the token-bearing /health URL.
async function startRT(t, extraEnv = {}) {
  const port = randomPort();
  const home = scratchHome();
  const raw = spawnRaw({
    port,
    home,
    env: { FLEETDECK_REQUIRE_TOKEN: 'on', FLEETDECK_TOKEN: TOKEN, ...extraEnv },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const baseUrl = `http://127.0.0.1:${port}`;
  await waitForResponse(`${baseUrl}/health?t=${encodeURIComponent(TOKEN)}`);
  return { raw, home, port, baseUrl };
}

// Resolve with the first snapshot frame from a WS that is expected to connect.
function snapshot(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => { ws.terminate(); reject(new Error('timed out waiting for snapshot')); }, scaleMs(5000));
    ws.once('message', raw => {
      clearTimeout(timer);
      try { resolve(JSON.parse(raw.toString('utf8'))); } catch (err) { reject(err); }
      ws.close();
    });
    ws.once('error', err => { clearTimeout(timer); reject(err); });
  });
}

// Resolve when a WS that is expected to be refused is torn down (never opens).
function refused(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => { ws.terminate(); reject(new Error('unauthenticated WebSocket was not closed')); }, scaleMs(5000));
    ws.once('open', () => { clearTimeout(timer); ws.terminate(); reject(new Error('unauthenticated WebSocket unexpectedly opened')); });
    ws.once('error', () => { clearTimeout(timer); resolve(); });
    ws.once('close', () => { clearTimeout(timer); resolve(); });
  });
}

test('REQUIRE_TOKEN=on: loopback exemption survives only for hooks and the shell', async t => {
  const { baseUrl } = await startRT(t);

  // A data route is now gated on loopback — the 401 the standalone auth-failure
  // path already returns everywhere else.
  const noTok = await fetch(`${baseUrl}/state`);
  assert.equal(noTok.status, 401, 'a tokenless loopback data route must be gated under the flag');
  assert.deepEqual(await noTok.json(), { ok: false, reason: 'unauthorized' });

  const bearer = await fetch(`${baseUrl}/state`, { headers: { authorization: `Bearer ${TOKEN}` } });
  assert.equal(bearer.status, 200, 'the bearer token must authorize on loopback');

  const query = await fetch(`${baseUrl}/state?t=${encodeURIComponent(TOKEN)}`);
  assert.equal(query.status, 200, 'the ?t= query token must also authorize');

  // The exec route this whole feature exists for: no tokenless spawn on a
  // shared box, even from loopback.
  const spawn = await fetch(`${baseUrl}/api/spawn`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ cwd: '/tmp' }),
  });
  assert.equal(spawn.status, 401, 'tokenless spawn must be refused even on loopback under the flag');

  // A /hook/* path keeps the exemption: Claude Code http-type hooks cannot send
  // an Authorization header, so gating them would break every session on the box.
  const sid = randomUUID();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-rt-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const hook = await fetch(`${baseUrl}/hook/SessionStart`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(loadFixture('session-start', { session_id: sid, cwd })),
  });
  assert.equal(hook.status, 200, 'a tokenless hook must still be accepted under the flag');
  assert.equal((await hook.json()).ok, true);

  // The data-free public shell stays open: a browser must load it before it can
  // present a key at all.
  const shell = await fetch(`${baseUrl}/`);
  assert.equal(shell.status, 200, 'the data-free shell must load tokenless under the flag');
});

test('REQUIRE_TOKEN=on: the /ws snapshot upgrade needs the token even on loopback', async t => {
  const { baseUrl } = await startRT(t);
  const wsBase = baseUrl.replace(/^http:/, 'ws:');

  await refused(`${wsBase}/ws`); // no token → destroyed before the upgrade completes
  const frame = await snapshot(`${wsBase}/ws?t=${encodeURIComponent(TOKEN)}`);
  assert.equal(frame.type, 'snapshot', 'the ?t= token must open the snapshot socket on loopback');
});

test('REQUIRE_TOKEN=on generates and persists a loopback token (0600) with no FLEETDECK_TOKEN', async t => {
  // Proves the token-generation path runs regardless of LAN_MODE: the flag alone
  // (loopback bind, no explicit token) mints + persists one, then gates on it.
  const home = scratchHome();
  const port = randomPort();
  const raw = spawnRaw({ port, home, env: { FLEETDECK_REQUIRE_TOKEN: 'on' } });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const tokenFile = path.join(home, 'token');
  const deadline = Date.now() + scaleMs(5000);
  while (!existsSync(tokenFile) && Date.now() < deadline) await sleep(25);
  assert.equal(existsSync(tokenFile), true, `token was not generated on loopback; stderr: ${raw.stderr}`);
  assert.equal(statSync(tokenFile).mode & 0o777, 0o600, 'a generated token must stay owner-only');
  const generated = readFileSync(tokenFile, 'utf8').trim();
  assert.match(generated, /^[0-9a-f]{64}$/);

  const baseUrl = `http://127.0.0.1:${port}`;
  await waitForResponse(`${baseUrl}/health?t=${encodeURIComponent(generated)}`);

  const noTok = await fetch(`${baseUrl}/state`);
  assert.equal(noTok.status, 401, 'the generated token must gate a loopback data route');
  const withTok = await fetch(`${baseUrl}/state`, { headers: { authorization: `Bearer ${generated}` } });
  assert.equal(withTok.status, 200, 'the generated token must authorize a loopback data route');
});

test('flag off (default): a loopback data route stays open with no token', async t => {
  // TODAY'S BEHAVIOR PINNED. The gate is additive — with FLEETDECK_REQUIRE_TOKEN
  // unset the loopback exemption must be byte-for-byte as before.
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const state = await fetch(`${daemon.baseUrl}/state`);
  assert.equal(state.status, 200, 'default loopback must keep tokenless /state');
  assert.ok(Array.isArray((await state.json()).sessions));
});
