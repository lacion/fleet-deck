// tests/require-token.test.mjs
//
// FLEETDECK_REQUIRE_TOKEN=on — opt into the token even on pure loopback. On a
// multi-user machine every other OS user can reach 127.0.0.1 and today inherits
// the loopback auto-authorize (tokenless /state, /api/spawn, the lot). With the
// flag on that exemption survives only for the data-free public shell (a
// browser must load it before it can present a key) and /health. Everything
// else — data routes, exec routes, both WebSockets, and (since 0.16.0, in EVERY
// mode) /hook/* — falls through to the bearer/?t= check even on loopback:
// hooks now arrive through the command shims, which attach $FLEETDECK_HOME/token.
//
// The gate under test lives in http.mjs authorized(); it is additive, so the
// LAST test pins that with the flag OFF a loopback data route stays wide open
// (today's behavior), guaranteeing this never regresses the default.

import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
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

// A loopback request with FULL control over the Host/Origin headers. undici's
// fetch silently DROPS a Host override (a forbidden request header there), so a
// proxy-shaped request — a loopback socket carrying the PROXY's external Host —
// cannot be crafted with fetch. node:http honours the override, so the trusted-
// proxy path is actually exercised. Resolves { status, body }; never rejects on
// a non-2xx. (Mirrors tests/lan-auth.test.mjs rawGet.)
function rawGet(port, pathname, headers = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request(
      { host: '127.0.0.1', port, path: pathname, method: 'GET', headers },
      res => {
        let body = '';
        res.on('data', c => { body += c; });
        res.on('end', () => resolve({ status: res.statusCode, body }));
      },
    );
    req.on('error', reject);
    req.end();
  });
}

test('REQUIRE_TOKEN=on: loopback exemption survives only for /health and the shell', async t => {
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

  // 0.16.0 INVERSION: /hook/* no longer keeps the exemption — hooks arrive
  // through the command shims (scripts/fleet-hook.mjs et al.), which read
  // $FLEETDECK_HOME/token and attach it. A tokenless hook is exactly the
  // forgery the shims exist to stop, so it 401s; the bearer opens it.
  const sid = randomUUID();
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-rt-cwd-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const hookBody = JSON.stringify(loadFixture('session-start', { session_id: sid, cwd }));
  const bareHook = await fetch(`${baseUrl}/hook/SessionStart`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: hookBody,
  });
  assert.equal(bareHook.status, 401, 'a tokenless hook must be refused in every mode');
  const hook = await fetch(`${baseUrl}/hook/SessionStart`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${TOKEN}` },
    body: hookBody,
  });
  assert.equal(hook.status, 200, 'the bearer must open /hook/* under the flag');
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

test('REQUIRE_TOKEN=on: an ENV-supplied FLEETDECK_TOKEN is persisted to HOME/token (0600)', async t => {
  // FIX: the generate path only writes HOME/token when it MINTS a token. When
  // the operator PINS FLEETDECK_TOKEN in the env (how startRT starts the daemon,
  // and the documented way), no file was written — but fleet-watch.mjs /
  // fleet-sessionstart.mjs read the bearer ONLY from HOME/token, so with no file
  // they present nothing and every gated call 401s. The token must be persisted
  // even when it came from the env. The daemon writes it synchronously before it
  // listens, so once /health answers (startRT waits on it) the file must exist.
  const { home } = await startRT(t);
  const tokenFile = path.join(home, 'token');
  assert.equal(existsSync(tokenFile), true, 'an env-supplied token must be persisted for file-only clients (fleet-watch)');
  assert.equal(readFileSync(tokenFile, 'utf8').trim(), TOKEN, 'the persisted file must hold the env-supplied token verbatim');
  assert.equal(statSync(tokenFile).mode & 0o777, 0o600, 'the persisted token must stay owner-only');
});

test('REQUIRE_TOKEN=on: /health stays tokenless (liveness/version probe)', async t => {
  // /health is deliberately ABSENT from the CHANGELOG'd gated list: it is a
  // liveness/version probe used by `fleetdeck status`, the supervisor's
  // waitForHealth and the standalone check — all tokenless by design — and it
  // carries no fleet data. The flag must not gate it.
  const { baseUrl } = await startRT(t);

  const health = await fetch(`${baseUrl}/health`);
  assert.equal(health.status, 200, '/health must not be gated by REQUIRE_TOKEN');
  const body = await health.json();
  assert.equal(body.ok, true);
  assert.equal(typeof body.version, 'string');

  // A neighbouring data route stays gated — /health is the only added carve-out.
  const state = await fetch(`${baseUrl}/state`);
  assert.equal(state.status, 401, 'a data route must stay gated while /health is exempt');
});

test('REQUIRE_TOKEN=on + PROXY_AUTH=trust: a browser via the trusted proxy needs no token', async t => {
  // PROXY_AUTH=trust makes the reverse proxy the authenticator, so a browser
  // that genuinely arrived through the trusted proxy carries NO bearer. The
  // requireToken tightening must not fire for it: REQUIRE_TOKEN closes the
  // LOOPBACK trust zone against other OS users, it does NOT override an explicit
  // operator decision to trust a proxy. Before the fix this browser 401'd with
  // no token to present.
  const PROXY_HOST = 'board.example.com';
  const port = randomPort();
  const home = scratchHome();
  const raw = spawnRaw({
    port,
    home,
    env: {
      FLEETDECK_REQUIRE_TOKEN: 'on',
      FLEETDECK_TOKEN: TOKEN,
      FLEETDECK_PROXY_AUTH: 'trust',
      FLEETDECK_TRUSTED_ORIGINS: `https://${PROXY_HOST}`,
    },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const baseUrl = `http://127.0.0.1:${port}`;
  await waitForResponse(`${baseUrl}/health?t=${encodeURIComponent(TOKEN)}`);

  // A browser arriving through the trusted proxy: the proxy's external Host, its
  // trusted Origin, and NO bearer. It must be authorized despite REQUIRE_TOKEN.
  const proxied = await rawGet(port, '/state', { Host: PROXY_HOST, Origin: `https://${PROXY_HOST}` });
  assert.equal(proxied.status, 200, 'a trusted-proxy browser must not be gated by REQUIRE_TOKEN in trust mode');

  // The Host-only variant (no Origin) is still recognised as arriving via the
  // proxy (arrivedViaTrustedProxy's Host-based signal) and stays exempt too.
  const proxiedNoOrigin = await rawGet(port, '/state', { Host: PROXY_HOST });
  assert.equal(proxiedNoOrigin.status, 200, 'the Host-based trusted-proxy signal must also stay exempt in trust mode');

  // But a PLAIN loopback data route — our own Host, no proxy, no token — is
  // still gated by the flag: trust mode does not reopen the loopback trust zone.
  const plain = await rawGet(port, '/state', { Host: `127.0.0.1:${port}` });
  assert.equal(plain.status, 401, 'plain loopback stays gated under REQUIRE_TOKEN even in trust mode');
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
