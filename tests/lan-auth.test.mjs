import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import http, { createServer } from 'node:http';
import path from 'node:path';
import { WebSocket } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.mjs';
import { waitForResponse, nonInternalIpv4s, scaleMs } from './helpers/wait.mjs';

const LAN_TOKEN = 'fleetdeck-lan-test-token-0123456789';

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
    }, scaleMs(5000));
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
    }, scaleMs(5000));
    ws.once('open', () => {
      clearTimeout(timer);
      ws.terminate();
      reject(new Error('unauthenticated WebSocket unexpectedly opened'));
    });
    ws.once('error', () => { clearTimeout(timer); resolve(); });
    ws.once('close', () => { clearTimeout(timer); resolve(); });
  });
}

// A loopback request with FULL control over the Host header. undici's fetch
// silently DROPS a Host override (it is a forbidden request header there), so a
// proxy-shaped request — a loopback socket carrying the PROXY's external Host —
// cannot be crafted with fetch: the header would fall back to 127.0.0.1:port and
// the request would look local, quietly passing whether or not the fix is
// present. node:http honours the override, so the proxy hole is actually
// exercised. Resolves { status, body } and never rejects on a non-2xx.
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

test('default bind preserves unauthenticated loopback health and hook traffic', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());

  const health = await fetch(`${daemon.baseUrl}/health`);
  assert.equal(health.status, 200);
  assert.equal((await health.json()).ok, true);

  // LOCAL-HOOK REGRESSION CONTRACT: hooks reach the daemon through the command
  // shims, which read $FLEETDECK_HOME/token and attach it — so hook traffic
  // stays zero-config (never knows a LAN token) AND keeps its fail-open
  // response. 0.16.0: the bearer comes from the daemon's own token file.
  const hook = await fetch(`${daemon.baseUrl}/hook/UnknownHook`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${daemon.token}` },
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
  //    The src is RELATIVE ("./assets/…") since the board learned to live under a
  //    reverse-proxy path prefix — so resolve it the way a browser would rather
  //    than assuming it is rooted. What is pinned here is unchanged: whatever the
  //    shell points at must load with no token.
  const asset = /src="([^"]+\.js)"/.exec(html)?.[1];
  assert.ok(asset, `the shell must reference a hashed module (got: ${html.slice(0, 200)})`);
  const script = await fetch(new URL(asset, `${daemon.baseUrl}/`));
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

test('proxy token mode: a loopback request bearing a trusted proxy Host but no Origin and no token is rejected', async t => {
  // NO-ORIGIN PROXY-HOLE REGRESSION (C1/H-S3). A reverse proxy connects to the
  // daemon over loopback. viaTrustedProxy keyed off the Origin header alone, so a
  // request that reached the public proxy carrying the proxy's trusted Host, NO
  // Origin and NO token looked exactly like a local CLI hook: isLoopbackAddress
  // true, viaTrustedProxy false, so the loopback exemption authorized it — every
  // control route (spawn=RCE, /state, mail, cleanup) exposed tokenless. The fix
  // (arrivedViaTrustedProxy) adds a Host-based signal: the proxy's EXTERNAL Host
  // is authorityTrusted but NOT hostAllowed, so such a request must still clear
  // the token check. This pins that it does.
  const PROXY_HOST = 'board.example.com';
  const daemon = await startDaemon({
    env: {
      FLEETDECK_TRUSTED_ORIGINS: `https://${PROXY_HOST}`,
      FLEETDECK_PROXY_AUTH: 'token',
      FLEETDECK_TOKEN: LAN_TOKEN,
    },
  });
  t.after(() => daemon.stop());

  // THE FIX: proxied, no Origin, no token → must fall through to the token check
  // and 401, NOT be waved through as a local hook. Before the fix this was 200.
  const attack = await rawGet(daemon.port, '/state', { Host: PROXY_HOST });
  assert.equal(attack.status, 401, 'a proxied no-Origin no-token request must be rejected, not treated as a local hook');

  // A genuine local CLI hook — our own loopback Host, no Origin, no token — is
  // untouched: hostAllowed ⇒ not via the proxy ⇒ the loopback exemption stands.
  const localHook = await rawGet(daemon.port, '/state', { Host: `127.0.0.1:${daemon.port}` });
  assert.equal(localHook.status, 200, 'a local loopback request must remain authorized with no token');

  // The same proxied request, now carrying the bearer token, is authorized.
  const withToken = await rawGet(daemon.port, '/state', { Host: PROXY_HOST, authorization: `Bearer ${LAN_TOKEN}` });
  assert.equal(withToken.status, 200, 'a proxied request that presents the token must be authorized');
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

  const deadline = Date.now() + scaleMs(5000);
  while (!existsSync(tokenFile) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 25));
  }
  assert.equal(existsSync(tokenFile), true, `token was not created; stderr: ${raw.stderr}`);
  assert.equal(statSync(tokenFile).mode & 0o777, 0o600);
  const generated = readFileSync(tokenFile, 'utf8').trim();
  assert.match(generated, /^[0-9a-f]{64}$/);
});
