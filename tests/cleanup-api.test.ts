import test from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { startDaemon } from './helpers/daemon.ts';
import { postHook, postJson, getJson } from './helpers/http.ts';
import { waitUntil } from './helpers/wait.ts';
import { openDb } from '../scripts/fleetd/db.ts';
import type { StateResponse } from '../contracts/state.ts';

interface CleanupResponse {
  ok: boolean;
  archived: number;
  conflicts_cleared: number;
  feed_cleared: number;
  mail_expired: number;
  orphan_worktrees: unknown[];
  questions_expired: number;
  questions_purged: number;
  windows_killed: number;
}
interface CleanupBlocked {
  ok: boolean;
  reason: string;
}
interface DismissResponse {
  ok: boolean;
  retry: boolean;
  archived: number;
  reason: string;
}

test('POST /api/cleanup archives offline sessions and expires their queued mail', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
  });
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { session_id: sid, cwd: process.cwd(), source: 'startup' },
    { token: daemon.token },
  );
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    { session_id: sid, cwd: process.cwd(), reason: 'done' },
    { token: daemon.token },
  );
  await postJson(
    `${daemon.baseUrl}/mail`,
    { to: sid, from: 'ops', text: 'undeliverable' },
    { token: daemon.token },
  );

  const res = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(res.status, 200);
  const body = res.json as CleanupResponse;
  assert.deepEqual(Object.keys(body).sort(), [
    'archived',
    'conflicts_cleared',
    'feed_cleared',
    'mail_expired',
    'ok',
    'orphan_worktrees',
    'questions_expired',
    'questions_purged',
    'windows_killed',
  ]);
  assert.equal(body.ok, true);
  assert.equal(body.archived, 1);
  assert.equal(body.mail_expired, 1);
  assert.equal(body.questions_expired, 0);
  assert.ok(Array.isArray(body.orphan_worktrees));
  const state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.equal(
    state.sessions.some((s) => s.session_id === sid),
    false,
  );
});

test('Clear wipes everything that is not alive: conflicts, the rail, the feed', async (t) => {
  const daemon = await startDaemon();
  t.after(async () => {
    await daemon.stop();
  });

  // Two sessions in one repo argue over a file, then BOTH die.
  const dead1 = randomUUID();
  const dead2 = randomUUID();
  for (const sid of [dead1, dead2]) {
    await postHook(
      daemon.baseUrl,
      'SessionStart',
      { session_id: sid, cwd: process.cwd(), source: 'startup' },
      { token: daemon.token },
    );
    await postHook(
      daemon.baseUrl,
      'PostToolUse',
      {
        session_id: sid,
        cwd: process.cwd(),
        tool_name: 'Edit',
        tool_input: { file_path: `${process.cwd()}/contested.js` },
      },
      { token: daemon.token },
    );
  }
  let state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.ok(state.conflicts.length >= 1, 'sanity: the radar raised a conflict');
  assert.ok(
    state.conflicts[0]?.callsigns.length,
    'a conflict must name callsigns, never raw uuids',
  );
  assert.ok(state.ticker.length > 0, 'sanity: the feed has narration in it');

  // A THIRD session is still alive and arguing with nobody — it must survive.
  const alive = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { session_id: alive, cwd: process.cwd(), source: 'startup' },
    { token: daemon.token },
  );

  for (const sid of [dead1, dead2]) {
    await postHook(
      daemon.baseUrl,
      'SessionEnd',
      { session_id: sid, cwd: process.cwd(), reason: 'done' },
      { token: daemon.token },
    );
  }

  const res = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(res.status, 200);
  const body = res.json as CleanupResponse;
  assert.ok(body.conflicts_cleared >= 1, 'a conflict between two dead sessions is not news');

  state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.equal(state.conflicts.length, 0, 'the conflict banner must not outlive its sessions');
  assert.equal(
    state.questions.filter((q) => q.status !== 'pending').length,
    0,
    'the rail keeps no ghosts',
  );
  assert.ok(state.ticker.length <= 1, 'the feed is wiped (bar the line announcing the wipe)');
  assert.ok(
    state.sessions.some((s) => s.session_id === alive),
    'the living are never cleared',
  );
  assert.equal(
    state.sessions.some((s) => s.session_id === dead1),
    false,
    'the dead are',
  );
});

// BUG-145: cleanup used to treat a null tmux listing as an empty fleet and
// dismiss ignored failed window kills, both answering unconditional success
// while stale windows stayed hidden with no retry path. This stands a REAL
// dead remain-on-exit window on the test daemon's scoped tmux server, then
// kills that server mid-flight: with no claim/death-certificate for that
// server the listing is UNKNOWN (never claimed → no `authoritativeEmpty`),
// so Clear must 409 with nothing archived, and a dismiss whose kill cannot be
// verified must answer 409 + retry:true (the /dismiss/retry route
// re-attempts — and also fails loud while tmux is down).
test('BUG-145: Clear and dismiss fail loud when tmux is unreachable, with a retry path', async (t) => {
  // HOME is set so the daemon's generation verification is ENABLED — with HOME
  // unset (and no other HOME to claim), a silent socket passes as a verified
  // empty listing, which would mask exactly the UNKNOWN path this test proves.
  const daemon = await startDaemon({ env: { HOME: process.env['HOME'] ?? '/tmp' } });
  t.after(async () => {
    await daemon.stop();
  });
  const socket = `fleetdeck-test-${daemon.port}`;
  const tmux = (...args: string[]): Buffer | string =>
    execFileSync('tmux', ['-L', socket, ...args], { stdio: ['ignore', 'pipe', 'pipe'] });
  const winName = `fd${daemon.port}-z145`;

  // An offline card that owns a terminal ('pane-dead') spawn row naming a
  // window that really exists on the scoped server — stood up BY HAND (the
  // daemon never claims the server, so its death leaves NO death certificate
  // and the listing is UNKNOWN, not authoritatively empty).
  const sid = randomUUID();
  await postHook(
    daemon.baseUrl,
    'SessionStart',
    { session_id: sid, cwd: process.cwd(), source: 'startup' },
    { token: daemon.token },
  );
  await postHook(
    daemon.baseUrl,
    'SessionEnd',
    { session_id: sid, cwd: process.cwd(), reason: 'done' },
    { token: daemon.token },
  );
  {
    // Same open/close DB idiom the revive tests use (a writer held open
    // across daemon requests deadlocks WAL).
    const db = openDb(path.join(daemon.home, 'fleetd.db'));
    try {
      db.prepare(
        `INSERT INTO spawns
        (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
        VALUES ('sp-145', ?, 'z145', ?, ?, 0, 'pane-dead')`,
      ).run(sid, `fleetdeck-${daemon.port}`, winName);
    } finally {
      db.close();
    }
  }
  tmux('new-session', '-d', '-s', `fleetdeck-${daemon.port}`, '-n', winName);
  // remain-on-exit keeps the pane standing after its command exits — the exact
  // corpse shape cleanup/dismiss are responsible for killing. Session scope,
  // so it is in force for the window that already exists.
  tmux('set-option', 'remain-on-exit', 'on');
  tmux('send-keys', '-t', winName, 'exit', 'Enter');
  await waitUntil(
    () => {
      try {
        return (
          execFileSync(
            'tmux',
            ['-L', socket, 'display-message', '-p', '-t', winName, '#{pane_dead}'],
            { encoding: 'utf8' },
          ).trim() === '1' || null
        );
      } catch {
        return null;
      }
    },
    { label: 'a dead remain-on-exit pane stands on the scoped server' },
  );

  // tmux dies: the listing becomes UNKNOWN. Clear must NOT report success —
  // and must not have archived anything (the whole Clear is the retry).
  execFileSync('tmux', ['-L', socket, 'kill-server'], { stdio: 'ignore' });

  const blocked = await postJson(`${daemon.baseUrl}/api/cleanup`, {});
  assert.equal(
    blocked.status,
    409,
    'an UNKNOWN tmux listing is a non-2xx incomplete, not a success',
  );
  const blockedBody = blocked.json as CleanupBlocked;
  assert.equal(blockedBody.ok, false);
  assert.match(blockedBody.reason, /listing unavailable/);
  let state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.ok(
    state.sessions.some((s) => s.session_id === sid),
    'nothing was archived — Clear is fully retryable',
  );

  // Dismiss lands its DB story but reports the incomplete window kill
  // honestly instead of hiding the stale window behind a 200.
  const dis = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/dismiss`, {});
  assert.equal(dis.status, 409, 'the partial dismiss is not a 200');
  const disBody = dis.json as DismissResponse;
  assert.equal(disBody.ok, false);
  assert.equal(disBody.retry, true, 'the failure carries its retry path');
  assert.equal(disBody.archived, 1, 'the DB story did land');
  assert.match(disBody.reason, /listing unavailable/);

  // The retry route re-attempts — and also fails loud while tmux is down.
  const retried = await postJson(`${daemon.baseUrl}/api/sessions/${sid}/dismiss/retry`, {});
  assert.equal(retried.status, 409);
  assert.equal((retried.json as DismissResponse).retry, true);
  const unknown = await postJson(
    `${daemon.baseUrl}/api/sessions/${randomUUID()}/dismiss/retry`,
    {},
  );
  assert.equal(unknown.status, 404);

  state = (await getJson(`${daemon.baseUrl}/state`)).json as StateResponse;
  assert.equal(
    state.sessions.some((s) => s.session_id === sid),
    false,
    'the card is archived by the partial dismiss',
  );
});
