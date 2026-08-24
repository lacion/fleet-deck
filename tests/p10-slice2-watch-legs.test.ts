// tests/p10-slice2-watch-legs.test.ts
//
// P10 SLICE 2 — EFFECT-PATH pins for the two GET /api/watch await legs that
// watch-rewake.test.ts (the "64 s oracle") never exercises. watch-rewake pins
// the WAKE (leg 1) and HOLD-TIMER (leg 2) arms with real timers; it never drives
// a shutdown or a mid-poll client disconnect. Design §1E enumerates four await
// legs; these pin the remaining two.
//
// THESE RUN ON THE EFFECT PARK, not the legacy rollback. The fixture
// (tests/helpers/http-lifecycle-effect-fixture.ts) calls installEffectRoutes
// exactly as the live daemon does (program.ts:884), so a parked GET /api/watch
// arms heldSettleWorkflow (src/daemon/app/http-workflows/held.ts), discharges it
// through the production runControlDetached runner, and folds its HeldOutcome to
// the wire via settleEffectWatchHold (http.ts). The legacy imperative park is the
// rollback seam (effectRoutes null) and is frozen by the in-process byte-
// equivalence pin at the bottom of this file — the two sides answer identically.
//
// PROOF THE EFFECT PARK IS TAKEN. runHeld is reachable ONLY from
// settleEffectWatchHold (the `if (effectRoutes)` branch of watchHook); the legacy
// park never touches it. The fixture reports `effectRoutesInstalled` at readiness
// and a `runHeldCalls` counter with every `counts`/`closed`, so each leg below
// asserts runHeldCalls === 1 — a watch that fell through to the legacy park would
// leave it 0.
//
//   1E-3  Shutdown closer     -> a parked watch is settled by closeClientsOnce's
//                                activeWatchClosers loop, which calls the closer
//                                leg's settle() with the HARDCODED idle body
//                                {status:'idle', session_alive:false, pending:0}
//                                — NOT a fresh watchInfo (contrast the hold-timer
//                                leg, which re-reads watchInfo). The session is
//                                LIVE, so a fresh read would report
//                                session_alive:true; the closer's hardcoded false
//                                is what proves leg 3 (not leg 2) settled it.
//   1E-4  Socket disconnect   -> a client abort on a parked watch calls abandon()
//                                (res.on('close')); the Deferred resolves
//                                {_tag:'abandon'} and settleEffectWatchHold writes
//                                NOTHING. The abandon retires the shutdown closer
//                                (watchWaiters 1->0) via the acquireRelease
//                                teardown, while the response stays lifecycle-owned
//                                until shutdown's forceEnd clears it (mirrors the
//                                abandoned-hook oracle in http-lifecycle).
//
// Both run against tests/helpers/http-lifecycle-effect-fixture.ts — a real
// createHttp/createCore subprocess with the full Effect port installed — because
// activeWatchClosers + ownedCounts are transport concerns, not core/questions
// ones. `counts` reads ownedCounts (watchWaiters === activeWatchClosers.size,
// activeResponses) + runHeldCalls; `close` drives http.lifecycle.close() →
// closeClientsOnce.
//
// Wire dialect (design §1E): GET /api/watch is ALWAYS 200 with an idle-poll body
// — NOT the hook fail-open {} contract. These pins freeze that distinct terminal.

import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import http from 'node:http';
import net, { type AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import * as Effect from 'effect/Effect';

import { createCore } from '../src/daemon/derive.ts';
import { openDb } from '../src/daemon/db.ts';
import { createHttp } from '../src/daemon/http.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import {
  armUnsupervisedWorkflow,
  controlAsyncWorkflow,
  controlSyncWorkflow,
  mailAckWorkflow,
  mailDrainWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
  spawnRouteWorkflow,
} from '../src/daemon/app/http-workflows/control.ts';
import {
  healthWorkflow,
  settingsSnapshotWorkflow,
  stateWorkflow,
} from '../src/daemon/app/http-workflows/health-state.ts';
import { hookDispatchWorkflow } from '../src/daemon/app/http-workflows/hooks.ts';
import { pasteImageWorkflow } from '../src/daemon/app/http-workflows/paste.ts';
import {
  cleanupWorkflow,
  commandWorkflow,
  mailWorkflow,
  settingsWorkflow,
} from '../src/daemon/app/http-workflows/settings-command-mail-cleanup.ts';
import {
  worktreeRemoveWorkflow,
  worktreesSnapshotWorkflow,
} from '../src/daemon/app/http-workflows/worktrees.ts';
import { repoPreflightWorkflow } from '../src/daemon/app/http-workflows/repos.ts';
import { heldSettleWorkflow } from '../src/daemon/app/http-workflows/held.ts';

import test from './helpers/harness-test.ts';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..');
const FIXTURE = path.join(HERE, 'helpers/http-lifecycle-effect-fixture.ts');

// The fixture reports these owned resources; a clean lifecycle close zeroes them.
const ZERO_OWNED_COUNTS = {
  listener: 0,
  snapshotClients: 0,
  terminalClients: 0,
  activeResponses: 0,
  watchWaiters: 0,
  terminalOpens: 0,
  broadcastTimers: 0,
  keepaliveTimers: 0,
};

interface FixtureReady {
  type: 'ready';
  port: number;
  pid: number;
  // Proves the fixture installed the Effect port (program.ts:884 does this
  // unconditionally in the live daemon); a legacy fixture would never report it.
  effectRoutesInstalled: boolean;
}
interface FixtureClosed {
  type: 'closed';
  sharedClosePromise: boolean;
  // Times runHeld was invoked — reachable ONLY from settleEffectWatchHold, so a
  // non-zero count proves the watch parked through the Effect held primitive.
  runHeldCalls: number;
  ownedCounts: typeof ZERO_OWNED_COUNTS;
}
interface FixtureCounts {
  type: 'counts';
  runHeldCalls: number;
  ownedCounts: typeof ZERO_OWNED_COUNTS;
}
type FixtureMessage = FixtureReady | FixtureClosed | FixtureCounts;

async function freePort(): Promise<number> {
  const probe = http.createServer();
  await new Promise<void>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', resolve);
  });
  const port = (probe.address() as AddressInfo).port;
  await new Promise<void>((resolve) => probe.close(() => resolve()));
  return port;
}

function within<T>(promise: Promise<T>, label: string, timeoutMs = 5_000): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${label}`)), timeoutMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

async function* fixtureMessages(
  stream: ReadableStream<Uint8Array>,
): AsyncGenerator<FixtureMessage> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  let buffered = '';
  try {
    while (true) {
      const { done, value } = await reader.read();
      buffered += decoder.decode(value, { stream: !done });
      let newline = buffered.indexOf('\n');
      while (newline >= 0) {
        const line = buffered.slice(0, newline).trim();
        buffered = buffered.slice(newline + 1);
        if (line) yield JSON.parse(line) as FixtureMessage;
        newline = buffered.indexOf('\n');
      }
      if (done) {
        const tail = buffered.trim();
        if (tail) yield JSON.parse(tail) as FixtureMessage;
        return;
      }
    }
  } finally {
    reader.releaseLock();
  }
}

async function nextFixtureMessage(
  iterator: AsyncIterator<FixtureMessage>,
  label: string,
): Promise<FixtureMessage> {
  const next = await within(iterator.next(), label);
  if (next.done) throw new Error(`fixture exited before ${label}`);
  return next.value;
}

interface HookResponse {
  status: number;
  body: unknown;
}

// A normal (fast) hook POST over the daemon's HTTP surface — used to SessionStart
// so watchInfo reports session_alive:true (session row exists, ended_at null).
function hook(base: string, token: string, body: Record<string, unknown>): Promise<HookResponse> {
  const payload = JSON.stringify(body);
  const url = new URL(`/hook/${String(body['hook_event_name'])}`, base);
  return new Promise<HookResponse>((resolve, reject) => {
    const request = http.request(
      url,
      {
        method: 'POST',
        agent: false,
        headers: {
          authorization: `Bearer ${token}`,
          connection: 'close',
          'content-type': 'application/json',
          'content-length': Buffer.byteLength(payload),
        },
      },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.once('error', reject);
        response.once('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          resolve({ status: response.statusCode ?? 0, body: text ? JSON.parse(text) : null });
        });
      },
    );
    request.once('error', reject);
    request.end(payload);
  });
}

interface WatchSocket {
  socket: net.Socket;
  // Resolves ONCE the server writes a complete HTTP response (headers +
  // content-length body). A parked watch never resolves this until a leg fires.
  response: Promise<HookResponse>;
  // Total bytes the server has written to this socket so far (0 while parked).
  received: () => number;
}

// Park a raw GET /api/watch long-poll. keep-alive + a manual write mirror
// openHeldHookSocket in http-lifecycle.test.ts: a GET has no body, so it drains
// immediately and the response is withheld until a settle leg calls json().
async function openWatchSocket(
  port: number,
  token: string,
  sid: string,
  holdMs: number,
): Promise<WatchSocket> {
  const socket = net.createConnection({ host: '127.0.0.1', port });
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', resolve);
    socket.once('error', reject);
  });

  let buffer = Buffer.alloc(0);
  const response = new Promise<HookResponse>((resolve, reject) => {
    let settled = false;
    const tryParse = (): void => {
      if (settled) return;
      const boundary = buffer.indexOf('\r\n\r\n');
      if (boundary < 0) return;
      const head = buffer.subarray(0, boundary).toString('utf8');
      const status = /^HTTP\/1\.1\s+(\d+)/.exec(head)?.[1];
      const contentLength = /content-length:\s*(\d+)/i.exec(head)?.[1];
      if (!status || contentLength === undefined) return;
      const bodyStart = boundary + 4;
      const need = Number(contentLength);
      if (buffer.length - bodyStart < need) return;
      settled = true;
      const text = buffer.subarray(bodyStart, bodyStart + need).toString('utf8');
      resolve({ status: Number(status), body: text ? JSON.parse(text) : null });
    };
    socket.on('data', (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      tryParse();
    });
    socket.once('error', (err) => {
      if (!settled) reject(err);
    });
  });

  socket.write(
    [
      `GET /api/watch?session=${encodeURIComponent(sid)}&hold_ms=${holdMs} HTTP/1.1`,
      `Host: 127.0.0.1:${port}`,
      `Authorization: Bearer ${token}`,
      'Connection: keep-alive',
      '',
      '',
    ].join('\r\n'),
  );

  return { socket, response, received: () => buffer.length };
}

async function bootFixture(t: { after: (fn: () => void | Promise<void>) => void }): Promise<{
  base: string;
  port: number;
  token: string;
  messages: AsyncIterator<FixtureMessage>;
  send: (command: 'counts' | 'close') => Promise<void>;
}> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p10-watch-'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const child = Bun.spawn([process.execPath, FIXTURE, home, String(port), token], {
    cwd: ROOT,
    env: { ...process.env, FLEETDECK_TERM: 'off' },
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stderr = Bun.readableStreamToText(child.stderr);
  const messages = fixtureMessages(child.stdout)[Symbol.asyncIterator]();
  t.after(async () => {
    try {
      await child.stdin.end();
    } catch {
      /* already closed */
    }
    try {
      await within(child.exited, 'p10 watch fixture exit', 2_000);
    } catch {
      child.kill('SIGKILL');
      await child.exited;
    }
    void stderr;
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const ready = await nextFixtureMessage(messages, 'p10 watch fixture readiness');
  assert.equal(ready.type, 'ready');
  assert.equal(ready.port, port);
  assert.equal(
    ready.effectRoutesInstalled,
    true,
    'the fixture installed the Effect port — these legs run on the Effect park, not the legacy rollback',
  );

  const send = async (command: 'counts' | 'close'): Promise<void> => {
    child.stdin.write(`${command}\n`);
    await child.stdin.flush();
  };

  return { base, port, token, messages, send };
}

// ======================= U3 — IN-PROCESS ROLLBACK-SEAM PIN =======================
//
// Finding 1 proves the Effect park answers correctly; this locks the OTHER side of
// the seam. Rollback is `effectRoutes` left null (http.ts: the `if (effectRoutes)`
// branch of watchHook is skipped, so the legacy imperative park answers). The
// prior converted families all freeze that seam in-process with a byte-equivalence
// pin (settings convention: `effectRoutes=null is the rollback seam` +
// assertByteIdentical). Slice 2 adds the watch equivalent here: two in-process
// boards — one WITHOUT installEffectRoutes (legacy park) and one WITH the full
// Effect port (effect park) — must answer GET /api/watch's two terminal-only legs
// (shutdown-closer idle body, socket-disconnect no-write) BYTE-IDENTICALLY. That is
// what makes the retained legacy park a faithful rollback of the Effect path, not
// just frozen source. Both sides settle through the same `json(res,200,…)` helper,
// so content-type / nosniff / content-length are compared too, not just the body.

type LifecycleHandle = ReturnType<typeof createHttp>['lifecycle'];

// The full Effect port minus runRequest (set inline). runHeld is the REAL
// production runner (runControlDetached, untracked) and watchHold is the real
// heldSettleWorkflow — the same wiring program.ts installs — so the effect board's
// watch route is the genuine held primitive, not a stub.
const WATCH_PORT_BUILDERS = {
  health: healthWorkflow,
  state: stateWorkflow,
  settingsSnapshot: settingsSnapshotWorkflow,
  settings: settingsWorkflow,
  command: commandWorkflow,
  mail: mailWorkflow,
  cleanup: cleanupWorkflow,
  pasteImage: pasteImageWorkflow,
  controlAsync: controlAsyncWorkflow,
  controlSync: controlSyncWorkflow,
  questionsDismiss: questionsDismissWorkflow,
  nameControl: nameControlWorkflow,
  armUnsupervised: armUnsupervisedWorkflow,
  mailAck: mailAckWorkflow,
  mailDrain: mailDrainWorkflow,
  spawnRoute: spawnRouteWorkflow,
  hookDispatch: hookDispatchWorkflow,
  worktreesSnapshot: worktreesSnapshotWorkflow,
  repoPreflight: repoPreflightWorkflow,
  worktreeRemove: worktreeRemoveWorkflow,
  runHeld: runControlDetached,
  watchHold: heldSettleWorkflow,
} as const;

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

// A full-response GET that collects headers + body (mirrors the settings-suite
// rawFull). Unlike hook()/openWatchSocket it exposes content-type / nosniff /
// content-length so byte-equivalence can compare the whole terminal, not just body.
function rawFull(
  port: number,
  reqPath: string,
  headers: Record<string, string>,
): Promise<RawResponse> {
  return new Promise<RawResponse>((resolve, reject) => {
    const request = http.request(
      { host: '127.0.0.1', port, path: reqPath, method: 'GET', headers },
      (response) => {
        let text = '';
        response.on('data', (chunk: Buffer) => {
          text += chunk.toString('utf8');
        });
        response.once('end', () => {
          resolve({ status: response.statusCode, headers: response.headers, body: text });
        });
      },
    );
    request.setTimeout(5_000, () => request.destroy(new Error('raw watch request timed out')));
    request.once('error', reject);
    request.end();
  });
}

function assertByteIdentical(actual: RawResponse, expected: RawResponse, label: string): void {
  assert.equal(actual.status, expected.status, `${label}: status`);
  assert.equal(actual.body, expected.body, `${label}: body`);
  assert.equal(
    actual.headers['content-type'],
    expected.headers['content-type'],
    `${label}: content-type`,
  );
  assert.equal(
    actual.headers['x-content-type-options'],
    expected.headers['x-content-type-options'],
    `${label}: nosniff`,
  );
  assert.equal(
    actual.headers['content-length'],
    expected.headers['content-length'],
    `${label}: content-length`,
  );
}

interface InProcBoard {
  port: number;
  base: string;
  token: string;
  lifecycle: LifecycleHandle;
}

// A real createHttp/createCore board in THIS process (no subprocess), so the test
// can read lifecycle.ownedCounts() directly and drive lifecycle.close() to fire the
// shutdown closer. installRoutes=false leaves effectRoutes null (the rollback park);
// installRoutes=true wires the full Effect port (the same shape program.ts uses).
async function startInProcBoard(
  t: { after: (fn: () => void | Promise<void>) => void },
  installRoutes: boolean,
): Promise<InProcBoard> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p10-inproc-'));
  const db = openDb(path.join(home, 'fleetd.db'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const core = createCore(db, { port, home, holdMs: 30_000, version: '0.0.0-test' });
  const handle = createHttp(core, { port, token, version: '0.0.0-test' });
  if (installRoutes) {
    handle.installEffectRoutes({
      runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
      ...WATCH_PORT_BUILDERS,
    });
  }
  await new Promise<void>((resolve, reject) => {
    handle.server.once('error', reject);
    handle.server.listen(port, '127.0.0.1', resolve);
  });
  let closed = false;
  t.after(async () => {
    if (!closed) {
      closed = true;
      await handle.lifecycle.close();
    }
    await core.lifecycle.close();
    db.close();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { port, base: `http://127.0.0.1:${port}`, token, lifecycle: handle.lifecycle };
}

// Park a raw GET /api/watch, fire the shutdown closer (lifecycle.close()), and
// capture the settled terminal. The closer's hardcoded idle body is session-
// independent, so this is deterministic on both parks.
async function captureCloserIdle(board: InProcBoard, sid: string): Promise<RawResponse> {
  const started = await hook(board.base, board.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');
  const pending = rawFull(
    board.port,
    `/api/watch?session=${encodeURIComponent(sid)}&hold_ms=25000`,
    {
      authorization: `Bearer ${board.token}`,
      connection: 'close',
    },
  );
  await Bun.sleep(150);
  await board.lifecycle.close();
  return within(pending, `${sid} closer idle`);
}

// Park a raw watch on a socket we control, drop the peer, and report the observable
// aftermath: bytes written (must stay 0), watchWaiters (closer must retire to 0),
// activeResponses (the response stays lifecycle-owned at 1 until shutdown).
async function captureDisconnectNoWrite(
  board: InProcBoard,
  sid: string,
): Promise<{ received: number; waitersAfter: number; activeAfter: number }> {
  const started = await hook(board.base, board.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');
  const watch = await openWatchSocket(board.port, board.token, sid, 25_000);
  await Bun.sleep(150);
  const parked = board.lifecycle.ownedCounts();
  assert.equal(parked.activeResponses, 1, 'the watch long-poll is a tracked response');
  assert.equal(parked.watchWaiters, 1, 'the watch registers a shutdown closer');
  assert.equal(watch.received(), 0, 'a parked watch has written no bytes yet');
  watch.socket.destroy();
  await Bun.sleep(150);
  const after = board.lifecycle.ownedCounts();
  return {
    received: watch.received(),
    waitersAfter: after.watchWaiters,
    activeAfter: after.activeResponses,
  };
}

test('P10 1E-3 shutdown closer settles a parked watch to the HARDCODED idle body {status:idle, session_alive:false, pending:0}', async (t) => {
  const { base, port, token, messages, send } = await bootFixture(t);

  const sid = 'p10-watch-shutdown-closer';
  const started = await hook(base, token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');

  // hold_ms 25_000 keeps the hold-timer (leg 2) dormant for the whole test, so
  // only the shutdown closer (leg 3) can settle this poll.
  const watch = await openWatchSocket(port, token, sid, 25_000);
  await Bun.sleep(150);

  await send('counts');
  const parked = await nextFixtureMessage(messages, 'watch parked counts');
  assert.equal(parked.type, 'counts');
  assert.equal(parked.ownedCounts.activeResponses, 1, 'the watch long-poll is a tracked response');
  assert.equal(parked.ownedCounts.watchWaiters, 1, 'the watch registers a shutdown closer');
  assert.equal(
    parked.runHeldCalls,
    1,
    'the parked watch armed the Effect held primitive (watchHook effectRoutes branch -> settleEffectWatchHold -> runHeld); a legacy-park watch would leave this 0',
  );
  assert.equal(watch.received(), 0, 'a parked watch has written no bytes yet');

  await send('close');
  const closed = await nextFixtureMessage(messages, 'watch shutdown lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.deepEqual(
    closed.ownedCounts,
    ZERO_OWNED_COUNTS,
    'shutdown settles the watch and releases every owned resource',
  );

  const settled = await within(watch.response, 'watch shutdown idle body');
  assert.equal(settled.status, 200, 'GET /api/watch always answers 200 — even on shutdown');
  assert.deepEqual(
    settled.body,
    { status: 'idle', session_alive: false, pending: 0 },
    'the shutdown closer writes the HARDCODED idle body (session_alive:false despite a LIVE session), not a fresh watchInfo',
  );

  watch.socket.destroy();
});

test('P10 1E-4 a mid-poll client disconnect writes NOTHING and retires the shutdown closer (watchWaiters 1->0)', async (t) => {
  const { base, port, token, messages, send } = await bootFixture(t);

  const sid = 'p10-watch-socket-disconnect';
  const started = await hook(base, token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');

  const watch = await openWatchSocket(port, token, sid, 25_000);
  await Bun.sleep(150);

  await send('counts');
  const parked = await nextFixtureMessage(messages, 'watch parked counts');
  assert.equal(parked.type, 'counts');
  assert.equal(parked.ownedCounts.activeResponses, 1, 'the watch long-poll is a tracked response');
  assert.equal(parked.ownedCounts.watchWaiters, 1, 'the watch registers a shutdown closer');
  assert.equal(
    parked.runHeldCalls,
    1,
    'the parked watch armed the Effect held primitive (watchHook effectRoutes branch -> settleEffectWatchHold -> runHeld); a legacy-park watch would leave this 0',
  );
  assert.equal(watch.received(), 0, 'a parked watch has written no bytes yet');

  // The peer vanishes before any leg fires. On the EFFECT park res.on('close')
  // calls abandon(), which resolves the Deferred with {_tag:'abandon'}. The
  // acquireRelease finalizer (the arm closure's teardown) then runs BEFORE the
  // settler observes the outcome — clearing the 25s timer, unregistering the
  // waiter, and deleting the shutdown closer (watchWaiters 1->0). Because the
  // outcome is 'abandon', settleEffectWatchHold writes NOTHING (contrast every
  // settle -> json path). The response stays lifecycle-owned until shutdown's
  // forceEnd retires it. The socket-close leg is the watch analog of
  // hold-manager 1A.3.
  watch.socket.destroy();
  await Bun.sleep(150);

  await send('counts');
  const afterClose = await nextFixtureMessage(messages, 'watch post-disconnect counts');
  assert.equal(afterClose.type, 'counts');
  assert.equal(
    afterClose.ownedCounts.watchWaiters,
    0,
    'the disconnect retires the shutdown closer (abandon runs the acquireRelease teardown)',
  );
  assert.equal(
    afterClose.ownedCounts.activeResponses,
    1,
    'the disconnected watch response stays lifecycle-owned until shutdown settles it',
  );
  assert.equal(
    afterClose.runHeldCalls,
    1,
    'the abandon resolved the SAME Effect hold (runHeld still 1) — no legacy park ran as a fallback',
  );
  assert.equal(watch.received(), 0, 'the socket-close leg wrote nothing to the parked watch');

  // Shutdown still drains cleanly: the abandoned response is force-settled and
  // every owned resource returns to zero, with no closer left to fire.
  await send('close');
  const closed = await nextFixtureMessage(messages, 'watch post-disconnect lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.deepEqual(
    closed.ownedCounts,
    ZERO_OWNED_COUNTS,
    'shutdown clears the abandoned watch response and every owned resource',
  );
});

test('P10 1E-3 rollback seam: effectRoutes=null (legacy park) and the installed Effect park emit BYTE-IDENTICAL shutdown-closer idle bytes', async (t) => {
  // Both parks answer the shutdown closer through the same json(res,200,…) helper,
  // so the whole terminal (status/body/content-type/nosniff/content-length) must
  // match. This is the watch equivalent of the settings-suite rollback-seam pin —
  // together with the effect-park pin above it locks BOTH sides of the seam.
  const legacyBoard = await startInProcBoard(t, false);
  const effectBoard = await startInProcBoard(t, true);

  const legacyIdle = await captureCloserIdle(legacyBoard, 'p10-inproc-legacy-closer');
  const effectIdle = await captureCloserIdle(effectBoard, 'p10-inproc-effect-closer');

  assert.equal(legacyIdle.status, 200, 'the legacy rollback park answers 200 on shutdown');
  assert.equal(
    legacyIdle.body,
    '{"status":"idle","session_alive":false,"pending":0}',
    'the shutdown closer writes the hardcoded idle body on the legacy park',
  );
  assertByteIdentical(
    effectIdle,
    legacyIdle,
    'GET /api/watch shutdown-closer idle: effect park == legacy rollback park',
  );
});

test('P10 1E-4 rollback seam: effectRoutes=null (legacy park) and the installed Effect park both write NOTHING on disconnect and retire the closer', async (t) => {
  // The socket-disconnect leg has no wire bytes to compare — its terminal is the
  // ABSENCE of a write plus the owned-resource aftermath. Both parks must leave the
  // response written-nothing, retire the shutdown closer (watchWaiters 1->0), and
  // keep the response lifecycle-owned (activeResponses 1) until shutdown.
  const legacyBoard = await startInProcBoard(t, false);
  const effectBoard = await startInProcBoard(t, true);

  const legacyDisc = await captureDisconnectNoWrite(legacyBoard, 'p10-inproc-legacy-disc');
  const effectDisc = await captureDisconnectNoWrite(effectBoard, 'p10-inproc-effect-disc');

  assert.deepEqual(
    legacyDisc,
    { received: 0, waitersAfter: 0, activeAfter: 1 },
    'the legacy rollback park writes nothing, retires the closer, and keeps the response owned',
  );
  assert.deepEqual(
    effectDisc,
    legacyDisc,
    'GET /api/watch disconnect no-write: effect park == legacy rollback park',
  );
});
