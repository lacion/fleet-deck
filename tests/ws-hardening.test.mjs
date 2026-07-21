// tests/ws-hardening.test.mjs
//
// M-P1 — broadcast coalescing: a burst of mutations must not rebuild and push a
// full snapshot per event. H-S1 — the LAN token must never ride the /ws
// broadcast/connect snapshot; it stays on the token-gated /state route only.

import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createServer } from 'node:http';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocket } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.mjs';
import { postHook } from './helpers/http.mjs';
import { loadFixture } from './helpers/fixtures.mjs';
import { waitUntil as waitUntilBase, waitForResponse, nonInternalIpv4s } from './helpers/wait.mjs';

const LAN_TOKEN = 'fleetdeck-ws-hardening-token-0123456789';

function connect(url, options) {
  const ws = new WebSocket(url, options);
  const frames = [];
  ws.on('message', raw => { try { frames.push(JSON.parse(raw.toString('utf8'))); } catch { /* junk */ } });
  return { ws, frames };
}

// Positional-signature adapter over the shared scaled poller: call sites pass
// (fn, label) with an authored 5000ms budget and a 20ms poll.
const waitUntil = (fn, label, timeoutMs = 5000) =>
  waitUntilBase(fn, { label, timeoutMs, intervalMs: 20 });

test('M-P1: a burst of mutations coalesces into far fewer broadcasts', async t => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-coalesce-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });

  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => ws.close());
  await waitUntil(() => frames.find(f => f.type === 'snapshot'), 'initial connect snapshot');

  // Fire many mutations as close to simultaneously as the loop allows. Each
  // PostToolUse bumps the session (events++, last_seen) → onMutate. Unbatched,
  // that was one full snapshot rebuild+stringify+send PER hook.
  const N = 40;
  const baseline = frames.length;
  await Promise.all(Array.from({ length: N }, () =>
    postHook(daemon.baseUrl, 'PostToolUse', loadFixture('post-tool-use-bash', { session_id: sid, cwd }), { token: daemon })));

  // Let the trailing coalesce window (and any straddling ones) flush.
  await new Promise(r => setTimeout(r, 400));
  const broadcasts = frames.length - baseline;

  assert.ok(broadcasts >= 1, 'clients must still converge — at least one snapshot after the burst');
  assert.ok(broadcasts <= 10, `expected the ${N} mutations to coalesce into far fewer broadcasts, saw ${broadcasts}`);
  assert.ok(broadcasts < N, 'coalescing must produce fewer broadcasts than mutations');

  // …and the board really did converge on the latest state.
  const last = [...frames].reverse().find(f => f.type === 'snapshot');
  const session = last.sessions.find(s => s.session_id === sid);
  assert.ok(session, 'the coalesced snapshot still carries the mutated session');
});

test('R1-2: a /ws client past the buffer cap is TERMINATED on broadcast, not silently skipped', async t => {
  // Deterministic eviction: with the cap forced to -1, bufferedAmount (always
  // >= 0) exceeds it for every peer, so the very next broadcast must terminate
  // the client. The bug was to instead SKIP the send while clearing `dirty` —
  // the mutation was then lost to a client that later recovers, because the
  // board stops /state polling while its socket is live. Terminate-and-reconnect
  // is the fix, and this pins it.
  const daemon = await startDaemon({ env: { FLEETDECK_WS_BUFFER_MAX: '-1' } });
  t.after(() => daemon.stop());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-evict-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));

  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => ws.close());
  let closed = false;
  ws.on('close', () => { closed = true; });
  // The connect snapshot still lands — the connection handler does not apply the
  // cap, only broadcast() does.
  await waitUntil(() => frames.find(f => f.type === 'snapshot'), 'connect snapshot');
  assert.equal(closed, false, 'a fresh client must not be evicted before any broadcast');

  // Any mutation drives a broadcast → the over-cap client is terminated.
  const sid = randomUUID();
  await postHook(daemon.baseUrl, 'SessionStart', loadFixture('session-start', { session_id: sid, cwd }), { token: daemon });
  await waitUntil(() => closed, 'over-cap client terminated on the broadcast');

  // …and the point of terminating: a reconnecting client is handed a COMPLETE
  // snapshot on connect, so it recovers the mutation it would otherwise never
  // have learned about. (The connect handler does not cap, so this frame lands.)
  const again = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => again.ws.close());
  const fresh = await waitUntil(
    () => again.frames.find(f => f.type === 'snapshot' && f.sessions?.some(s => s.session_id === sid)),
    'fresh connect snapshot carries the mutation the evicted client missed',
  );
  assert.ok(fresh, 'a reconnecting client recovers the state via the connect snapshot');
});

// ---- H-S1 needs a real LAN bind so a token exists at all. Skip when the host
// has no reachable non-internal IPv4 (CI, a locked-down sandbox), exactly like
// tests/lan-auth.test.mjs.

async function reachableIpv4() {
  for (const address of nonInternalIpv4s()) {
    const probe = createServer((_req, res) => res.end('ok'));
    try {
      await new Promise((resolve, reject) => { probe.once('error', reject); probe.listen(0, address, resolve); });
      const res = await fetch(`http://${address}:${probe.address().port}/`, { signal: AbortSignal.timeout(750) });
      if (res.ok) return address;
    } catch { /* unroutable from here */ } finally {
      await new Promise(resolve => probe.close(resolve));
    }
  }
  return null;
}

test('H-S1: the /ws snapshot carries no token; /state (authorized) still does', async t => {
  const address = await reachableIpv4();
  if (!address) return t.skip('host has no non-internal IPv4 interface');

  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hs1-'));
  const port = randomPort();
  const raw = spawnRaw({ port, home, env: { FLEETDECK_BIND: address, FLEETDECK_TOKEN: LAN_TOKEN } });
  t.after(async () => { await raw.kill(); rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }); });
  const baseUrl = `http://${address}:${port}`;
  await waitForResponse(`${baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);

  // The authorized HTTP caller still gets the token-bearing share URL from
  // /state (this is what keeps the board's share panel working).
  const state = await fetch(`${baseUrl}/state`, { headers: { authorization: `Bearer ${LAN_TOKEN}` } }).then(r => r.json());
  assert.ok(Array.isArray(state.lan?.urls) && state.lan.urls.length, '/state must still carry lan.urls');
  assert.ok(state.lan.urls.some(u => u.includes(LAN_TOKEN)), 'the authorized /state must carry the share URL WITH the token');

  // The /ws snapshot must leak NOTHING: no token anywhere, and no lan block at all.
  const { ws, frames } = connect(`${baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(LAN_TOKEN)}`);
  t.after(() => ws.close());
  const frame = await waitUntil(() => frames.find(f => f.type === 'snapshot'), 'LAN /ws snapshot');
  assert.ok(!JSON.stringify(frame).includes(LAN_TOKEN), 'the /ws snapshot must never contain the token');
  assert.equal(frame.lan, undefined, 'the /ws snapshot must not carry lan at all — the token rides its urls');

  // H-S1 must also hold on a BROADCAST, not just the connect snapshot: every
  // mutation pushes a fresh snapshot to each live /ws client, and that frame is
  // built by the SAME broadcast() path — but a regression could reintroduce the
  // token there while leaving the connect snapshot clean. Drive one mutation and
  // inspect the pushed frame. (The hook rides the LAN address with the token in
  // the query; a fetch sends no Origin, so it clears the CSRF wall as a CLI would.)
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hs1-mut-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const sid = randomUUID();
  const posted = await fetch(`${baseUrl}/hook/SessionStart?t=${encodeURIComponent(LAN_TOKEN)}`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify(loadFixture('session-start', { session_id: sid, cwd })),
  });
  assert.equal(posted.status, 200);
  const broadcast = await waitUntil(
    () => frames.find(f => f.type === 'snapshot' && f.sessions?.some(s => s.session_id === sid)),
    'LAN /ws broadcast carrying the new session',
  );
  assert.ok(!JSON.stringify(broadcast).includes(LAN_TOKEN), 'the broadcast snapshot must never contain the token');
  assert.equal(broadcast.lan, undefined, 'the broadcast snapshot must not carry lan either');
});
