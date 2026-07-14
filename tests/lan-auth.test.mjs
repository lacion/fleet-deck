import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { networkInterfaces, tmpdir } from 'node:os';
import { createServer } from 'node:http';
import path from 'node:path';
import { WebSocket } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.mjs';

const LAN_TOKEN = 'fleetdeck-lan-test-token-0123456789';

// Every non-internal IPv4, in order. NOT just the first one: a machine can
// carry addresses it cannot talk to itself on — a Tailscale/VPN interface owned
// by another network stack (this is exactly what WSL2's mirrored networking
// hands you), where a connection to the address simply hangs. The caller probes
// them and uses the first that actually answers, so this suite exercises the
// auth gate rather than a routing quirk.
function nonInternalIpv4s() {
  const found = [];
  try {
    for (const entries of Object.values(networkInterfaces())) {
      for (const entry of entries || []) {
        if ((entry.family === 'IPv4' || entry.family === 4) && !entry.internal) found.push(entry.address);
      }
    }
  } catch { /* restricted sandboxes may deny interface enumeration */ }
  return found;
}

// The first candidate this host can actually reach ITSELF on: stand up a
// throwaway listener bound to the address and fetch it. Guessing from the
// interface list is not enough — a mirrored/VPN address can be present, bindable
// and still unroutable from here, which looks exactly like a hung test.
// Returns null when no LAN address works (CI, a locked-down sandbox) and the
// LAN tests must skip rather than fail.
async function reachableIpv4() {
  for (const address of nonInternalIpv4s()) {
    const probe = createServer((_req, res) => res.end('ok'));
    try {
      await new Promise((resolve, reject) => {
        probe.once('error', reject);
        probe.listen(0, address, resolve);
      });
      const url = `http://${address}:${probe.address().port}/`;
      const res = await fetch(url, { signal: AbortSignal.timeout(750) });
      if (res.ok) return address;
    } catch { /* unroutable from here — try the next */ } finally {
      await new Promise(resolve => probe.close(resolve));
    }
  }
  return null;
}

function scratchHome() {
  return mkdtempSync(path.join(tmpdir(), 'fleetdeck-lan-auth-'));
}

async function waitForResponse(url, options = {}, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await fetch(url, { ...options, signal: AbortSignal.timeout(500) });
    } catch (err) {
      lastError = err;
      await new Promise(resolve => setTimeout(resolve, 100));
    }
  }
  throw new Error(`daemon never answered ${url}: ${lastError?.message || 'timeout'}`);
}

async function startLan(t, address, { token = LAN_TOKEN, home = scratchHome() } = {}) {
  const port = randomPort();
  const raw = spawnRaw({
    port,
    home,
    env: {
      FLEETDECK_BIND: address,
      ...(token === null ? {} : { FLEETDECK_TOKEN: token }),
    },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { raw, home, port, baseUrl: `http://${address}:${port}` };
}

function snapshot(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => {
      ws.terminate();
      reject(new Error('timed out waiting for snapshot'));
    }, 5000);
    ws.once('message', raw => {
      clearTimeout(timer);
      try { resolve(JSON.parse(raw.toString('utf8'))); } catch (err) { reject(err); }
      ws.close();
    });
    ws.once('error', err => { clearTimeout(timer); reject(err); });
  });
}

function refused(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    const timer = setTimeout(() => {
      ws.terminate();
      reject(new Error('unauthenticated WebSocket was not closed'));
    }, 5000);
    ws.once('open', () => {
      clearTimeout(timer);
      ws.terminate();
      reject(new Error('unauthenticated WebSocket unexpectedly opened'));
    });
    ws.once('error', () => { clearTimeout(timer); resolve(); });
    ws.once('close', () => { clearTimeout(timer); resolve(); });
  });
}

test('default bind preserves unauthenticated loopback health and hook traffic', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const health = await fetch(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200);
  assert.equal((await health.json()).ok, true);

  // LOCAL-HOOK REGRESSION CONTRACT: real Claude hooks never know a LAN token;
  // their 127.0.0.1 traffic must retain the historical fail-open response.
  const hook = await fetch(`${daemon.baseUrl}/hook/UnknownHook`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: '{}',
  });
  assert.equal(hook.status, 200);
});

test('a LAN peer can actually LOAD the board: shell public, every byte of data gated', async t => {
  // REGRESSION PIN. Gating the static shell looks stricter and is in fact a
  // blank page: a browser cannot put a token on the <script> tag inside the
  // page it is loading, so `/?t=<token>` would serve HTML whose own module
  // 401s. Loopback bypasses the gate, so this failure is invisible locally and
  // only bites the remote peer the feature exists for. It has broken once.
  const address = await reachableIpv4();
  if (!address) return t.skip('host has no non-internal IPv4 interface');
  const daemon = await startLan(t, address);
  await waitForResponse(`${daemon.baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);

  // 1. the shell loads with the token in the URL, exactly as the printed link
  const page = await fetch(`${daemon.baseUrl}/?t=${encodeURIComponent(LAN_TOKEN)}`);
  assert.equal(page.status, 200, 'the printed LAN link must serve the board');
  const html = await page.text();

  // 2. …and the script tag INSIDE that page must load — with no token, because
  //    the browser cannot add one. This is the assertion that was missing.
  const asset = /src="(\/assets\/[^"]+\.js)"/.exec(html)?.[1];
  assert.ok(asset, `the shell must reference a hashed module (got: ${html.slice(0, 200)})`);
  const script = await fetch(`${daemon.baseUrl}${asset}`);
  assert.equal(script.status, 200, "the board's own script must load without a token, or the page is blank");

  // 3. the shell is a shell: no fleet data rides along with it
  assert.doesNotMatch(html, /callsign|session_id|"sessions"/i, 'the public shell must carry no fleet data');

  // 4. and everything that matters is still shut
  for (const path of ['/state', '/health', '/api/watch?session=x']) {
    const res = await fetch(`${daemon.baseUrl}${path}`);
    assert.equal(res.status, 401, `${path} must stay gated`);
  }
  const spawn = await fetch(`${daemon.baseUrl}/api/spawn`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ cwd: '/tmp' }),
  });
  assert.equal(spawn.status, 401, 'spawning must never be reachable without the token');
});

test('LAN HTTP requires the query or bearer token for a non-loopback peer', async t => {
  const address = await reachableIpv4();
  if (!address) return t.skip('host has no non-internal IPv4 interface');
  const daemon = await startLan(t, address);
  await waitForResponse(`${daemon.baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);

  // A connection to our own interface retains that interface as the peer
  // address. The 401 below is the end-to-end proof; if Node classified it as
  // loopback, this exact request would bypass auth and return 200.
  const missing = await fetch(`${daemon.baseUrl}/health`);
  assert.equal(missing.status, 401, 'own-LAN-IP connection must be non-loopback');
  assert.deepEqual(await missing.json(), { ok: false, reason: 'unauthorized' });

  const query = await fetch(`${daemon.baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);
  assert.equal(query.status, 200);

  const bearer = await fetch(`${daemon.baseUrl}/health`, {
    headers: { authorization: `Bearer ${LAN_TOKEN}` },
  });
  assert.equal(bearer.status, 200);

  const wrong = await fetch(`${daemon.baseUrl}/health?t=definitely-wrong-token`);
  assert.equal(wrong.status, 401);
});

test('LAN snapshot WebSocket rejects no token and accepts the query token', async t => {
  const address = await reachableIpv4();
  if (!address) return t.skip('host has no non-internal IPv4 interface');
  const daemon = await startLan(t, address);
  await waitForResponse(`${daemon.baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);
  const wsBase = daemon.baseUrl.replace(/^http:/, 'ws:');

  await refused(`${wsBase}/ws`);
  const frame = await snapshot(`${wsBase}/ws?t=${encodeURIComponent(LAN_TOKEN)}`);
  assert.equal(frame.type, 'snapshot');
});

test('FLEETDECK_TOKEN shorter than 16 trimmed characters refuses startup', async t => {
  const home = scratchHome();
  const raw = spawnRaw({
    port: randomPort(),
    home,
    env: { FLEETDECK_TOKEN: '   too-short   ' },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const code = await raw.waitForExit();
  assert.notEqual(code, 0);
  assert.match(raw.stderr, /FLEETDECK_TOKEN must be at least 16 characters/i);
  assert.doesNotMatch(raw.stderr, /too-short/);
});

test('LAN mode generates and persists an owner-only token', async t => {
  const home = scratchHome();
  const raw = spawnRaw({
    port: randomPort(),
    home,
    env: { FLEETDECK_BIND: '0.0.0.0' },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const tokenFile = path.join(home, 'token');

  const deadline = Date.now() + 5000;
  while (!existsSync(tokenFile) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  assert.equal(existsSync(tokenFile), true, `token was not created; stderr: ${raw.stderr}`);
  assert.equal(statSync(tokenFile).mode & 0o777, 0o600);
  const generated = readFileSync(tokenFile, 'utf8').trim();
  assert.match(generated, /^[0-9a-f]{64}$/);
});
