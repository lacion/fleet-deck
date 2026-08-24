#!/usr/bin/env bun

// P10 SLICE 4 — MIXED-LOAD SHUTDOWN MATRIX (the D2 exit gate, both hold types in
// ONE quiesce).
//
// Slice 4's design prescription (p10-design §3) is "the releaseHolds ->
// closeClients -> forceStop ordering (1G) under the Effect model: holds settle
// 200 {} even if Store or terminal teardown fails — the exit gate." That ordering
// is ALREADY Effect-owned by the P4 LifecycleCoordinator (ShutdownPhaseOrder is
// frozen: releasing-holds strictly before closing-clients/closing-http/
// closing-store), and the SETTLEMENT inside those phases is ALREADY Effect (slice
// 2 wired the watch closer -> settleEffectWatchHold; slice 3 wired the hook
// releaseAll -> settleEffectHookHold). The coordinator body stays imperative by
// the binding migration doctrine (§6-Q1: the hold-manager Maps stay imperative
// behind the policy adapter; the Deferred wraps SETTLEMENT ONLY). The
// failure-injection exit gate is already pinned at the phase layer
// (lifecycle-coordinator.test.ts "every phase failure is retained and later
// phases still execute" + daemon-resource-lifecycle.test.ts "a phase failure
// skips store retirement" and the forceStop-after-releaseHolds order pins). So
// slice 4 converts NO source — it collapses to pins, the P9.6 justified-non-
// conversion shape.
//
// The ONE thing slices 2 and 3 each pinned SEPARATELY but never TOGETHER: a
// single shutdown that has BOTH a parked hook hold AND a parked watch hold live at
// once. slice3 test "shutdown leg (D2)" parks exactly one hook; slice2 test
// "1E-3 shutdown closer" parks exactly one watch. Neither proves the two
// parameterized folds coexist through one quiesce — that the SAME shutdown settles
// a hook to its canonical fail-open {} in the releasing-holds phase AND a watch to
// its HARDCODED idle body in the closing-clients phase, with the listener (and
// every other owned resource) released only AFTER both. That is the D2 matrix, and
// this file is its pin (design candidates b + c).
//
// Subprocess-only against the shared Effect fixture (http-lifecycle-effect-fixture
// .ts), the same oracle slices 2 and 3 use. runHeldCalls is the proof signal: it
// increments ONLY inside the two Effect settlers (settleEffectWatchHold,
// settleEffectHookHold), so a mixed load that reaches runHeldCalls===2 proves BOTH
// surfaces armed the Effect held primitive — a legacy-park fallback on either
// would leave it short. The in-flight-mutating-write dimension of candidate (c) is
// already covered structurally: closeClientsOnce joins every activeResponses
// promise before the listener releases (pinned by http-lifecycle.test.ts's
// withheld-hook join and daemon-resource-lifecycle's closing-clients order), so
// this pin does not re-stage a fragile mid-flight write.

import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import http from 'node:http';
import net, { type AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import test from './helpers/harness-test.ts';
import { connectBoardClient, closeBoardClient } from './helpers/http.ts';
import type { WebSocket } from 'ws';

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
  effectRoutesInstalled: boolean;
}
interface FixtureClosed {
  type: 'closed';
  sharedClosePromise: boolean;
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

// A normal (fast) hook POST — used for SessionStart (so the session row exists and
// the hold-scope gate can admit it). Resolves on the response 'end'.
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

interface StateQuestion {
  id: number;
  session_id: string;
  kind: string;
  status: string;
  held: boolean;
}

// GET /state and return the questions array. Used to confirm the parked hook's
// permission question reached 'pending' before we assert the counts.
function stateQuestions(base: string, token: string): Promise<StateQuestion[]> {
  const url = new URL('/state', base);
  return new Promise<StateQuestion[]>((resolve, reject) => {
    const request = http.request(
      url,
      { method: 'GET', agent: false, headers: { authorization: `Bearer ${token}` } },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.once('error', reject);
        response.once('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          const parsed = text ? (JSON.parse(text) as { questions?: StateQuestion[] }) : {};
          resolve(parsed.questions ?? []);
        });
      },
    );
    request.once('error', reject);
    request.end();
  });
}

async function pendingPermission(
  base: string,
  token: string,
  sid: string,
  label: string,
): Promise<StateQuestion> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    const questions = await stateQuestions(base, token);
    const q = questions.find(
      (row) => row.session_id === sid && row.kind === 'permission' && row.status === 'pending',
    );
    if (q) return q;
    await Bun.sleep(25);
  }
  throw new Error(`timed out waiting for ${label}`);
}

interface HeldSocket {
  socket: net.Socket;
  // Resolves ONCE the server writes a complete HTTP response (headers +
  // content-length body). A parked hold never resolves this until a settle leg
  // calls json().
  response: Promise<HookResponse>;
  // Total bytes the server has written so far (0 while parked).
  received: () => number;
}

// Parse a single HTTP/1.1 response off a raw keep-alive socket (headers +
// content-length body). Shared by the hook and watch parkers — both withhold their
// response until a settle leg fires, so the parse only completes on settlement.
function parkedSocket(socket: net.Socket): {
  response: Promise<HookResponse>;
  received: () => number;
} {
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
  return { response, received: () => buffer.length };
}

// Park a raw POST /hook/PermissionRequest with a COMPLETE body (Content-Length
// matched) so the request drains immediately, leaving the response withheld until
// a settle leg fires. Mirrors openHookSocket in p10-slice3-hook-hold-legs.test.ts.
async function openHookSocket(port: number, token: string, sid: string): Promise<HeldSocket> {
  const socket = net.createConnection({ host: '127.0.0.1', port });
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', resolve);
    socket.once('error', reject);
  });

  const { response, received } = parkedSocket(socket);
  const payload = JSON.stringify({
    hook_event_name: 'PermissionRequest',
    session_id: sid,
    cwd: '/tmp',
    tool_name: 'Bash',
    tool_input: { command: 'rm -rf build/' },
  });
  socket.write(
    [
      'POST /hook/PermissionRequest HTTP/1.1',
      `Host: 127.0.0.1:${port}`,
      `Authorization: Bearer ${token}`,
      'Content-Type: application/json',
      `Content-Length: ${Buffer.byteLength(payload)}`,
      'Connection: keep-alive',
      '',
      payload,
    ].join('\r\n'),
  );

  return { socket, response, received };
}

// Park a raw GET /api/watch long-poll. A GET has no body, so it drains
// immediately and the response is withheld until a settle leg calls json().
// Mirrors openWatchSocket in p10-slice2-watch-legs.test.ts.
async function openWatchSocket(
  port: number,
  token: string,
  sid: string,
  holdMs: number,
): Promise<HeldSocket> {
  const socket = net.createConnection({ host: '127.0.0.1', port });
  await new Promise<void>((resolve, reject) => {
    socket.once('connect', resolve);
    socket.once('error', reject);
  });

  const { response, received } = parkedSocket(socket);
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

  return { socket, response, received };
}

interface BootedFixture {
  base: string;
  port: number;
  token: string;
  board: WebSocket;
  messages: AsyncIterator<FixtureMessage>;
  send: (command: 'counts' | 'close') => Promise<void>;
}

// Boot the Effect-path fixture WITH the hold scope broadened to 'all' and ONE
// authorized snapshot client attached, so a PermissionRequest for any registered
// session parks (shouldRelayQuestion is satisfied). This mirrors slice 3's boot;
// the watch long-poll has no such gate but is unaffected by the broadened scope.
async function bootFixture(t: {
  after: (fn: () => void | Promise<void>) => void;
}): Promise<BootedFixture> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p10-matrix-'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    FLEETDECK_TERM: 'off',
    FLEETDECK_HOLD_SCOPE: 'all',
  };
  // The hook settler must never inherit a shortened B2 floor from a sibling suite
  // in the same process; settleEffectHookHold does not arm HOOK_REPLY_FLOOR_MS.
  delete env['FLEETDECK_HOOK_REPLY_FLOOR_MS'];
  const child = Bun.spawn([process.execPath, FIXTURE, home, String(port), token], {
    cwd: ROOT,
    env,
    stdin: 'pipe',
    stdout: 'pipe',
    stderr: 'pipe',
  });
  const stderr = Bun.readableStreamToText(child.stderr);
  const messages = fixtureMessages(child.stdout)[Symbol.asyncIterator]();
  let board: WebSocket | null = null;
  t.after(async () => {
    if (board) await closeBoardClient(board);
    try {
      await child.stdin.end();
    } catch {
      /* already closed */
    }
    try {
      await within(child.exited, 'p10 matrix fixture exit', 2_000);
    } catch {
      child.kill('SIGKILL');
      await child.exited;
    }
    void stderr;
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const ready = await nextFixtureMessage(messages, 'p10 matrix fixture readiness');
  assert.equal(ready.type, 'ready');
  assert.equal(ready.port, port);
  assert.equal(
    ready.effectRoutesInstalled,
    true,
    'the fixture installed the Effect port — this matrix runs on the Effect park, not the legacy rollback',
  );

  board = await connectBoardClient(base, token);

  const send = async (command: 'counts' | 'close'): Promise<void> => {
    child.stdin.write(`${command}\n`);
    await child.stdin.flush();
  };

  return { base, port, token, board, messages, send };
}

async function registerSession(fx: BootedFixture, sid: string): Promise<void> {
  const started = await hook(fx.base, fx.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, `SessionStart registers a live session (${sid})`);
}

// ============================ THE MIXED-LOAD MATRIX PIN ============================

test('P10 slice4 shutdown matrix (D2): ONE quiesce settles a parked hook to 200 {} AND a parked watch to the idle body, releasing the listener only after both', async (t) => {
  const fx = await bootFixture(t);
  const hookSid = 'p10-matrix-hook';
  const watchSid = 'p10-matrix-watch';

  // Two independent registered sessions — one carries a hook hold, the other a
  // watch long-poll — so a single shutdown must fold BOTH surfaces at once.
  await registerSession(fx, hookSid);
  await registerSession(fx, watchSid);

  // --- Park the hook hold. openHookSocket drains the POST body immediately; the
  // response is withheld until a settle leg fires. ---
  const held = await openHookSocket(fx.port, fx.token, hookSid);
  await pendingPermission(fx.base, fx.token, hookSid, 'matrix parked permission');

  await fx.send('counts');
  const afterHook = await nextFixtureMessage(fx.messages, 'matrix hook-only counts');
  assert.equal(afterHook.type, 'counts');
  assert.equal(
    (afterHook as FixtureCounts).runHeldCalls,
    1,
    'the parked hook armed the Effect held primitive (holdHook effectRoutes branch -> settleEffectHookHold -> runHeld)',
  );
  assert.equal(
    (afterHook as FixtureCounts).ownedCounts.activeResponses,
    1,
    'only the hook is parked so far — one tracked response',
  );
  assert.equal(
    (afterHook as FixtureCounts).ownedCounts.watchWaiters,
    0,
    'a hook hold registers no watch closer',
  );
  assert.equal(held.received(), 0, 'a parked hook has written no bytes yet');

  // --- Park the watch long-poll on the SECOND session. hold_ms 25_000 keeps the
  // hold-timer (leg 2) dormant for the whole test, so ONLY the shutdown closer
  // (leg 3) can settle this poll. ---
  const watch = await openWatchSocket(fx.port, fx.token, watchSid, 25_000);
  await Bun.sleep(150);

  await fx.send('counts');
  const bothParked = await nextFixtureMessage(fx.messages, 'matrix both-parked counts');
  assert.equal(bothParked.type, 'counts');
  assert.equal(
    (bothParked as FixtureCounts).runHeldCalls,
    2,
    'the watch armed a SECOND Effect held primitive (watchHook effectRoutes branch -> settleEffectWatchHold -> runHeld); both hold types now live on the Effect park, neither on a legacy fallback',
  );
  assert.equal(
    (bothParked as FixtureCounts).ownedCounts.activeResponses,
    2,
    'hook + watch are both tracked responses under one lifecycle',
  );
  assert.equal(
    (bothParked as FixtureCounts).ownedCounts.watchWaiters,
    1,
    'the watch (not the hook) registers exactly one shutdown closer',
  );
  assert.equal(watch.received(), 0, 'a parked watch has written no bytes yet');

  // --- ONE shutdown. releaseHeldResponses (releasing-holds phase) fails the hook
  // open to {}; closeClientsOnce (closing-clients phase) drains the watch closer to
  // the idle body; the listener releases only after both, in the frozen
  // ShutdownPhaseOrder (releasing-holds < closing-clients < closing-http). ---
  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'matrix lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.equal(
    (closed as FixtureClosed).runHeldCalls,
    2,
    'the shutdown settled the SAME two Effect holds — no legacy park ran as a fallback for either surface',
  );
  assert.deepEqual(
    (closed as FixtureClosed).ownedCounts,
    ZERO_OWNED_COUNTS,
    'the mixed shutdown settles BOTH holds and releases every owned resource — the listener is released only after the joins complete (listener:0 in the post-close counts)',
  );

  // Both parked responses settled to their DISTINCT canonical bodies in the same
  // quiesce — the parameterized fold proof.
  const hookSettled = await within(held.response, 'matrix hook fail-open body');
  assert.equal(hookSettled.status, 200, 'a hook released on shutdown always answers 200');
  assert.deepEqual(
    hookSettled.body,
    {},
    'D2 shutdown fails the hook open — releaseAll respond({}) -> settleEffectHookHold json {} (NOT the watch idle body)',
  );

  const watchSettled = await within(watch.response, 'matrix watch idle body');
  assert.equal(watchSettled.status, 200, 'GET /api/watch always answers 200 — even on shutdown');
  assert.deepEqual(
    watchSettled.body,
    { status: 'idle', session_alive: false, pending: 0 },
    'the shutdown closer writes the HARDCODED idle body (session_alive:false despite a LIVE session) -> settleEffectWatchHold (NOT the hook fail-open {})',
  );

  held.socket.destroy();
  watch.socket.destroy();
});
