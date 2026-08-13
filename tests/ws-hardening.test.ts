// tests/ws-hardening.test.ts
//
// M-P1 — broadcast coalescing: a burst of mutations must not rebuild and push a
// full snapshot per event. H-S1 — the LAN token must never ride the /ws
// broadcast/connect snapshot; it stays on the token-gated /state route only.

import test, { type TestContext } from './helpers/harness-test.ts';
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocket, type RawData, type ClientOptions } from 'ws';
import { randomPort, spawnRaw, startDaemon } from './helpers/daemon.ts';
import { postHook } from './helpers/http.ts';
import { loadFixture } from './helpers/fixtures.ts';
import { waitUntil as waitUntilBase, waitForResponse, nonInternalIpv4s } from './helpers/wait.ts';
import { openDb } from '../scripts/fleetd/db.ts';

const LAN_TOKEN = 'fleetdeck-ws-hardening-token-0123456789';

interface SessionRow {
  session_id: string;
  events: number;
}
interface LegacyUpgrade {
  sessions?: string[];
  upgraded: number;
}
// A /ws frame as these tests read it: every field optional, because a raw
// parsed frame proves nothing about its shape. Sites that have already
// established a frame IS a snapshot narrow to SnapshotFrame.
interface WsFrame {
  type?: string;
  sessions?: SessionRow[];
  legacy_upgrade?: LegacyUpgrade;
  lan?: unknown;
}
interface SnapshotFrame extends WsFrame {
  sessions: SessionRow[];
  legacy_upgrade: LegacyUpgrade;
}
interface ConnectResult {
  ws: WebSocket;
  frames: WsFrame[];
}

function connect(url: string, options?: ClientOptions): ConnectResult {
  const ws = new WebSocket(url, options);
  const frames: WsFrame[] = [];
  ws.on('message', (raw: RawData) => {
    try {
      frames.push(JSON.parse((raw as Buffer).toString('utf8')) as WsFrame);
    } catch {
      /* junk */
    }
  });
  return { ws, frames };
}

// Positional-signature adapter over the shared scaled poller: call sites pass
// (fn, label) with an authored 5000ms budget and a 20ms poll.
const waitUntil = <T>(
  fn: () => T | Promise<T>,
  label: string,
  timeoutMs = 5000,
): Promise<NonNullable<Awaited<T>>> => waitUntilBase(fn, { label, timeoutMs, intervalMs: 20 });

// Boot reconciliation (reconcileSpawns + the boot retentionSweep) runs
// fire-and-forget from the listen callback, so startDaemon's /health wait does
// NOT mean the startup mutation window has closed — and those heals broadcast.
// A /ws client with zero tolerance for a broadcast it did not cause (the
// over-cap eviction test below, cap forced to -1) must wait out that window
// before connecting, or the trailing startup broadcast terminates it and the
// "not evicted before any broadcast" assertion flakes (BUG-066). The daemon
// reports 'settled' on /health once BOTH heals are done AND the coalesced
// flush they scheduled has drained.
async function waitForSettled(
  baseUrl: string,
  label = 'boot reconciliation settled',
): Promise<void> {
  await waitUntil(
    async () => {
      try {
        const res = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(500) });
        const body = (await res.json()) as { startup?: string } | null;
        return body?.startup === 'settled' ? true : null;
      } catch {
        return null;
      }
    },
    label,
    10000,
  );
}

test('M-P1: a burst of mutations coalesces into far fewer broadcasts', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-coalesce-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // A bare temp dir is NOT a git repo, so nothing outside the daemon (hook
  // repo-derivation, background ingest) can mutate the session and bump its
  // events counter — every events tick observed below must be one of OUR hooks.
  const sid = randomUUID();
  const started = await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon },
  );
  assert.equal(started.status, 200, 'the SessionStart hook must be accepted');

  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => { ws.close(); });
  const first = (await waitUntil(
    () => frames.find((f) => f.type === 'snapshot'),
    'initial connect snapshot',
  )) as SnapshotFrame;

  // Record the baseline AFTER the connect snapshot has landed: any startup or
  // SessionStart flush is already accounted for in `baselineEvents`, so the
  // burst below is the only source of new events.
  const baselineSession = first.sessions.find((s) => s.session_id === sid);
  assert.ok(baselineSession, 'the connect snapshot must carry the session');
  const baselineEvents = baselineSession.events;
  assert.equal(
    typeof baselineEvents,
    'number',
    'the snapshot must expose the session events counter',
  );

  // Fire many mutations as close to simultaneously as the loop allows. Each
  // PostToolUse bumps the session (events++, last_seen) → onMutate. Unbatched,
  // that was one full snapshot rebuild+stringify+send PER hook.
  const N = 40;
  const baseline = frames.length;
  const responses = await Promise.all(
    Array.from({ length: N }, () =>
      postHook(
        daemon.baseUrl,
        'PostToolUse',
        loadFixture('post-tool-use-bash', { session_id: sid, cwd }),
        { token: daemon },
      ),
    ),
  );

  // Every hook must actually have LANDED. The old test discarded the
  // responses, so a run where all 40 were refused (or mutation broadcasting
  // was broken) could still pass on an unrelated trailing snapshot.
  for (const [i, res] of responses.entries()) {
    assert.equal(
      res.status,
      200,
      `PostToolUse #${i} must be accepted, got ${res.status}: ${res.text}`,
    );
  }

  // Let the trailing coalesce window (and any straddling ones) flush.
  await new Promise((r) => setTimeout(r, 400));
  const broadcasts = frames.length - baseline;

  assert.ok(broadcasts >= 1, 'clients must still converge — at least one snapshot after the burst');
  assert.ok(
    broadcasts <= 10,
    `expected the ${N} mutations to coalesce into far fewer broadcasts, saw ${broadcasts}`,
  );
  assert.ok(broadcasts < N, 'coalescing must produce fewer broadcasts than mutations');

  // …and the board really did converge on the latest state: the final snapshot
  // must carry every one of the N mutations, not merely the session row that
  // already existed before the burst.
  const last = [...frames].reverse().find((f) => f.type === 'snapshot') as SnapshotFrame;
  const session = last.sessions.find((s) => s.session_id === sid);
  assert.ok(session, 'the coalesced snapshot still carries the mutated session');
  assert.equal(
    session.events,
    baselineEvents + N,
    `all ${N} burst mutations must land: expected events ${baselineEvents + N}, got ${session.events}`,
  );
});

test('R1-2: a /ws client past the buffer cap is TERMINATED on broadcast, not silently skipped', async (t: TestContext) => {
  // Deterministic eviction: with the cap forced to -1, bufferedAmount (always
  // >= 0) exceeds it for every peer, so the very next broadcast must terminate
  // the client. The bug was to instead SKIP the send while clearing `dirty` —
  // the mutation was then lost to a client that later recovers, because the
  // board stops /state polling while its socket is live. Terminate-and-reconnect
  // is the fix, and this pins it.
  const daemon = await startDaemon({ env: { FLEETDECK_WS_BUFFER_MAX: '-1' } });
  t.after(() => daemon.stop());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-evict-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // With the cap at -1 EVERY broadcast terminates this client — including the
  // trailing boot-reconciliation flush. Wait out the startup mutation window
  // so the only broadcast in play is the one this test causes (BUG-066).
  await waitForSettled(daemon.baseUrl);

  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => { ws.close(); });
  let closed = false;
  ws.on('close', () => {
    closed = true;
  });
  // The connect snapshot still lands — the connection handler does not apply the
  // cap, only broadcast() does.
  await waitUntil(() => frames.find((f) => f.type === 'snapshot'), 'connect snapshot');
  assert.equal(closed, false, 'a fresh client must not be evicted before any broadcast');

  // Any mutation drives a broadcast → the over-cap client is terminated.
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon },
  );
  await waitUntil(() => closed, 'over-cap client terminated on the broadcast');

  // …and the point of terminating: a reconnecting client is handed a COMPLETE
  // snapshot on connect, so it recovers the mutation it would otherwise never
  // have learned about. (The connect handler does not cap, so this frame lands.)
  const again = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => { again.ws.close(); });
  const fresh = await waitUntil(
    () =>
      again.frames.find(
        (f) => f.type === 'snapshot' && f.sessions?.some((s) => s.session_id === sid),
      ),
    'fresh connect snapshot carries the mutation the evicted client missed',
  );
  assert.ok(fresh, 'a reconnecting client recovers the state via the connect snapshot');
});

test('BUG-066: /health "settled" closes the startup mutation window — an over-cap client connecting after it is never caught by the boot broadcast', async (t: TestContext) => {
  // The race this pins: boot reconcileSpawns is fire-and-forget, and its
  // onMutate only SCHEDULES a coalesced broadcast that fires up to
  // BROADCAST_COALESCE_MS after the heals resolve. A strict /ws client (cap
  // forced to -1, so every broadcast terminates it) that connects the instant
  // startDaemon's /health wait returns can still be caught by that trailing
  // startup flush — evicted before the test's own mutation, exactly the flake
  // the over-cap test above used to hit. This reproduces the window
  // DETERMINISTICALLY: restart against a HOME holding a stale 'live' spawn row
  // whose tmux window is gone, so boot reconciliation unconditionally settles
  // the row and broadcasts. A correct 'settled' must not flip until that flush
  // has drained, so a client connecting after it survives to the test's own
  // mutation.
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-066-home-'));
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-066-cwd-'));
  t.after(() => {
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const port = randomPort();
  const sid = randomUUID();

  // First boot: seed a session + a 'live' spawn row pointing at a window that
  // will not exist at the second boot.
  const first = await startDaemon({ port, home });
  const db = openDb(path.join(home, 'fleetd.db'));
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions (session_id, callsign, cwd, col, note, events, started_at, last_seen, blocked_this_turn, source)
    VALUES (?, ?, ?, 'working', 'seeded for BUG-066', 0, ?, ?, 0, 'hooks')`,
  ).run(sid, 'bug066', cwd, now, now);
  db.prepare(
    `INSERT INTO spawns (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, requested_at, status, skip_permissions, remote_control, gateway, kind)
    VALUES (?, ?, ?, ?, ?, ?, ?, 'live', 0, 0, 0, 'claude')`,
  ).run(999001, sid, 'bug066', `fleetdeck-${port}`, `fd${port}-bug066`, cwd, now);
  db.close();
  await first.stop({ keepHome: true });

  // Second boot against the SAME home: boot reconciliation condemns the stale
  // row → onMutate → coalesced broadcast. Over-cap from birth so ANY broadcast
  // terminates the client.
  const daemon = await startDaemon({ port, home, env: { FLEETDECK_WS_BUFFER_MAX: '-1' } });
  t.after(() => daemon.stop());

  // The fix under test: 'settled' must not flip until the startup broadcast has
  // drained. Connect immediately after it does — there must be no residual
  // startup flush left to terminate this client.
  await waitForSettled(daemon.baseUrl, 'boot reconciliation settled after restart');
  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => { ws.close(); });
  let closed = false;
  ws.on('close', () => {
    closed = true;
  });
  await waitUntil(() => frames.find((f) => f.type === 'snapshot'), 'connect snapshot after settle');
  // Give any (incorrectly) trailing startup flush ample room to fire. On the
  // unfixed readiness this is where the over-cap client died.
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(closed, false, 'no startup broadcast may remain once /health reports settled');

  // …and the client is still functional: the test's OWN mutation must be the
  // thing that terminates it (the over-cap contract still holds).
  const sid2 = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid2, cwd }),
    { token: daemon },
  );
  await waitUntil(() => closed, 'over-cap client terminated by the test-driven broadcast');
});

// ---- H-S1 needs a real LAN bind so a token exists at all. Skip when the host
// has no reachable non-internal IPv4 (CI, a locked-down sandbox), exactly like
// tests/lan-auth.test.ts.

async function reachableIpv4(): Promise<string | null> {
  for (const address of nonInternalIpv4s()) {
    const probe = createServer((_req, res) => {
      res.end('ok');
    });
    try {
      await new Promise<void>((resolve, reject) => {
        probe.once('error', reject);
        probe.listen(0, address, () => {
          resolve();
        });
      });
      const info = probe.address() as AddressInfo;
      const res = await fetch(`http://${address}:${info.port}/`, {
        signal: AbortSignal.timeout(750),
      });
      if (res.ok) return address;
    } catch {
      /* unroutable from here */
    } finally {
      await new Promise<void>((resolve) => {
        probe.close(() => {
          resolve();
        });
      });
    }
  }
  return null;
}

test('H-S1: the /ws snapshot carries no token; /state (authorized) still does', async (t: TestContext) => {
  const address = await reachableIpv4();
  if (!address) {
    t.skip('host has no non-internal IPv4 interface');
    return;
  }

  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hs1-'));
  const port = randomPort();
  const raw = spawnRaw({
    port,
    home,
    env: { FLEETDECK_BIND: address, FLEETDECK_TOKEN: LAN_TOKEN },
  });
  t.after(async () => {
    await raw.kill();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const baseUrl = `http://${address}:${port}`;
  await waitForResponse(`${baseUrl}/health?t=${encodeURIComponent(LAN_TOKEN)}`);

  // The authorized HTTP caller still gets the token-bearing share URL from
  // /state (this is what keeps the board's share panel working).
  const state = (await fetch(`${baseUrl}/state`, {
    headers: { authorization: `Bearer ${LAN_TOKEN}` },
  }).then((r) => r.json())) as { lan?: { urls?: string[] } };
  const lanUrls = state.lan?.urls;
  assert.ok(Array.isArray(lanUrls) && lanUrls.length, '/state must still carry lan.urls');
  assert.ok(
    lanUrls.some((u) => u.includes(LAN_TOKEN)),
    'the authorized /state must carry the share URL WITH the token',
  );

  // The /ws snapshot must leak NOTHING: no token anywhere, and no lan block at all.
  const { ws, frames } = connect(
    `${baseUrl.replace(/^http/, 'ws')}/ws?t=${encodeURIComponent(LAN_TOKEN)}`,
  );
  t.after(() => { ws.close(); });
  const frame = await waitUntil(
    () => frames.find((f) => f.type === 'snapshot'),
    'LAN /ws snapshot',
  );
  assert.ok(
    !JSON.stringify(frame).includes(LAN_TOKEN),
    'the /ws snapshot must never contain the token',
  );
  assert.equal(
    frame.lan,
    undefined,
    'the /ws snapshot must not carry lan at all — the token rides its urls',
  );

  // H-S1 must also hold on a BROADCAST, not just the connect snapshot: every
  // mutation pushes a fresh snapshot to each live /ws client, and that frame is
  // built by the SAME broadcast() path — but a regression could reintroduce the
  // token there while leaving the connect snapshot clean. Drive one mutation and
  // inspect the pushed frame. (The hook rides the LAN address with the token in
  // the query; a fetch sends no Origin, so it clears the CSRF wall as a CLI would.)
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-hs1-mut-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const sid = randomUUID();
  const posted = await fetch(`${baseUrl}/hook/SessionStart?t=${encodeURIComponent(LAN_TOKEN)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(loadFixture('session-start', { session_id: sid, cwd })),
  });
  assert.equal(posted.status, 200);
  const broadcast = await waitUntil(
    () =>
      frames.find((f) => f.type === 'snapshot' && f.sessions?.some((s) => s.session_id === sid)),
    'LAN /ws broadcast carrying the new session',
  );
  assert.ok(
    !JSON.stringify(broadcast).includes(LAN_TOKEN),
    'the broadcast snapshot must never contain the token',
  );
  assert.equal(broadcast.lan, undefined, 'the broadcast snapshot must not carry lan either');
});

test('BUG-031: /ws frames carry legacy_upgrade and a tokenless hook pushes one live', async (t: TestContext) => {
  const daemon = await startDaemon();
  t.after(() => daemon.stop());
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-bug031-'));
  t.after(() => {
    rmSync(cwd, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  // Connect snapshot: a live board's FIRST frame is authoritative — if it
  // drops legacy_upgrade the restart banner is wiped the moment the socket
  // opens and (the board stops /state polling) never comes back.
  const { ws, frames } = connect(daemon.baseUrl.replace(/^http/, 'ws') + '/ws');
  t.after(() => { ws.close(); });
  const first = await waitUntil(
    () => frames.find((f) => f.type === 'snapshot'),
    'connect snapshot',
  );
  assert.deepEqual(
    first.legacy_upgrade,
    { sessions: [], upgraded: 0 },
    'the /ws connect snapshot must carry legacy_upgrade like /state does',
  );

  // A pre-0.16 session keeps posting tokenless hooks. That call changes no
  // session state (it is refused), so without an explicit push a live board
  // would never see the banner appear — the frame must arrive on the socket.
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'UserPromptSubmit',
    loadFixture('user-prompt-submit', { session_id: sid, cwd }),
  );
  const pushed = (await waitUntil(
    () => frames.find((f) => f.type === 'snapshot' && f.legacy_upgrade?.sessions?.includes(sid)),
    'broadcast listing the legacy session',
  )) as SnapshotFrame;
  assert.equal(pushed.legacy_upgrade.upgraded, 0);
  // The pushed frame is still the H-S1 clean shape: no lan block, no token.
  assert.equal(pushed.lan, undefined, 'the legacy push must not smuggle the lan block onto /ws');

  // The session restarts (its first AUTHENTICATED hook) → the next frame
  // clears the banner entry on the live socket, no /state poll involved.
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    loadFixture('session-start', { session_id: sid, cwd }),
    { token: daemon },
  );
  const healed = (await waitUntil(
    () =>
      frames.find(
        (f) =>
          f.type === 'snapshot' &&
          f.sessions?.some((s) => s.session_id === sid) &&
          !f.legacy_upgrade?.sessions?.includes(sid),
      ),
    'broadcast after the restart clears the legacy entry',
  )) as SnapshotFrame;
  assert.equal(healed.legacy_upgrade.upgraded, 1, 'reconnected count moved on the wire');
});
