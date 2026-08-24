// tests/p10-slice3-hook-hold-legs.test.ts
//
// P10 SLICE 3 — EFFECT-PATH pins for the hook HOLD relay (the fail-open held
// responses for permission/elicitation/choice hooks). Slice 3 moves the hold's
// SETTLEMENT onto the SAME parameterized Deferred primitive the watch long-poll
// earned in Slice 2 (heldSettleWorkflow), while the P1 hold manager
// (attachHold's holds Map, the hold-window timer, per-session cap eviction, the
// UX-2.1 re-arm chain, completedKeys) stays IMPERATIVE behind the policy adapter
// (design §6-Q1). The Deferred wraps settlement ONLY: attachHold(row, respond)
// where respond(obj) settles {body:obj}, so the manager keeps CHOOSING the body
// (the board's decision on answer(), the canonical {} on every fail-open leg via
// respondFailOpen) and the Deferred merely carries it to settleEffectHookHold.
//
// TERMINAL FOLD = HOOK FAIL-OPEN (NOT the watch idle-poll dialect). Every
// non-answer completion renders 200 {} while the transport is writable; the ONLY
// leg that carries a body is a board answer. settleEffectHookHold folds:
//   settle  -> json(res, 200, (value as HookResponse).body)   answer body | {}
//   abandon -> NO WRITE                                        socket gone (1A.3)
//   defect  -> json(res, 200, hookFailOpenBody())              impossible-by-construction
// The `{body}` wrapping IS the mapHookExit success shape realized STRUCTURALLY —
// a held workflow yields a HeldOutcome, not an Exit, so we do NOT call mapHookExit
// on the hold path (that would double-wrap {body:{body:obj}}). The failure-arm
// contract-tie is hookFailOpenBody() in hook-policy.ts, used by BOTH mapHookExit's
// failure arm AND settleEffectHookHold's .catch.
//
// THESE RUN ON THE EFFECT PARK, not the legacy rollback. The fixture
// (tests/helpers/http-lifecycle-effect-fixture.ts) calls installEffectRoutes
// exactly as the live daemon does (program.ts), so a parked /hook arms
// heldSettleWorkflow, discharges it through the production runControlDetached
// runner, and folds its HeldOutcome to the wire via settleEffectHookHold. The
// legacy imperative park is the rollback seam (effectRoutes null OR
// EFFECT_CORE_HOLD_RELAY false) and is frozen by the in-process byte-equivalence
// pins at the bottom of this file — the two parks answer identically.
//
// PROOF THE EFFECT PARK IS TAKEN. runHeld is reachable ONLY from the two held
// settlers — settleEffectWatchHold (Slice 2) and settleEffectHookHold (the
// `if (effectRoutes && EFFECT_CORE_HOLD_RELAY)` branch of holdHook, Slice 3). A
// legacy-park hook never touches it. The fixture reports `effectRoutesInstalled`
// at readiness and a `runHeldCalls` counter with every `counts`/`closed`, so each
// effect-park leg below asserts runHeldCalls === 1 — a hook that fell through to
// the legacy imperative park would leave it 0. (The counter is shared with the
// watch settler. Tests that park one hold assert runHeldCalls === 1; the
// cap-eviction pin parks five and asserts 5.)
//
// A hook parks ONLY when a board consumer is connected AND the hold scope admits
// the session (events.ts shouldRelayQuestion: boardConsumerAvailable() &&
// (scope==='all' || spawned)). So these boots set FLEETDECK_HOLD_SCOPE='all' and
// attach one authorized snapshot client — exactly what board-hold-presence and
// http-lifecycle do. Without a board the intake fails open immediately (that gate
// is pinned by board-hold-presence.test.ts and is not re-pinned here).
//
// The HTTP-layer legs (design §2C / §1A):
//   answer            -> the ONLY body-carrying leg; board POST /answer settles the
//                        decision wire {hookSpecificOutput:{...decision...}}.
//   shutdown (D2)     -> releaseAll settles every parked hold to 200 {} while the
//                        transport can still write.
//   socket disconnect -> res.on('close') calls socketClosed(id)+abandon(); the
//                        Deferred resolves {_tag:'abandon'} and settleEffectHookHold
//                        writes NOTHING and never re-arms (1A.3, §6-Q5).
//   board disconnect  -> the last snapshot client 1->0 fires failOpenAllHolds
//                        (responder-first); each parked hold settles 200 {} (1A.5).
//   cap eviction      -> 5th attachHold on one session settleExpired(oldest) →
//                        200 {} through the Effect park; the other four stay parked
//                        (1A.4). runHeldCalls === 5.
//   dismiss (design #7) -> board POST /api/questions/:id/dismiss respondFailOpen →
//                        200 {}. Design §1A #7 is dismiss / expireOnActivity /
//                        expireOrphans — NOT the pre-park quiescing/null-row early
//                        return in holdHook (http.ts), which never reaches the
//                        Effect branch. expireOrphans skips holds.has live sockets
//                        (no respond), so it has no HTTP-layer parked-hold arm.
//   floor anti-truncation -> a parked Effect hold is still open (unsettled, no
//                        bytes) after HOOK_REPLY_FLOOR_MS (5s). The floor lives
//                        only on settleEffectHookRoute; copying it onto the hold
//                        settler would truncate a 600s park.

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

// POST a JSON body to an authenticated /api route (the board answer). Resolves on
// 'end' with status + parsed body.
function postJson(
  base: string,
  reqPath: string,
  token: string,
  body: Record<string, unknown>,
): Promise<HookResponse> {
  const payload = JSON.stringify(body);
  const url = new URL(reqPath, base);
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

// GET /state and return the questions array (the board snapshot the answer route
// keys off). Used to find the parked permission question's id.
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

interface HookSocket {
  socket: net.Socket;
  // Resolves ONCE the server writes a complete HTTP response (headers + body). A
  // parked hook never resolves this until a settle leg calls json().
  response: Promise<HookResponse>;
  // Total bytes the server has written so far (0 while parked).
  received: () => number;
}

// Park a raw POST /hook/PermissionRequest with a COMPLETE body (Content-Length
// matched) so the request drains immediately, leaving the response withheld until
// a settle leg fires. Mirrors openHeldHookSocket in http-lifecycle.test.ts.
async function openHookSocket(
  port: number,
  token: string,
  sid: string,
  command = 'rm -rf build/',
): Promise<HookSocket> {
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

  const payload = JSON.stringify({
    hook_event_name: 'PermissionRequest',
    session_id: sid,
    cwd: '/tmp',
    tool_name: 'Bash',
    tool_input: { command },
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

  return { socket, response, received: () => buffer.length };
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
// session parks (shouldRelayQuestion is satisfied). Slice 2's bootFixture leaves
// the scope at its 'spawned' default because a watch long-poll has no such gate;
// hooks need this, so slice 3 keeps its own boot rather than perturbing slice 2's.
async function bootFixture(t: {
  after: (fn: () => void | Promise<void>) => void;
}): Promise<BootedFixture> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p10-hook-'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    FLEETDECK_TERM: 'off',
    FLEETDECK_HOLD_SCOPE: 'all',
  };
  // The hold settler must never inherit a shortened B2 floor from a sibling
  // suite in the same process. settleEffectHookHold deliberately does NOT arm
  // HOOK_REPLY_FLOOR_MS (default 5000); the anti-truncation pin below waits
  // past that default.
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
      await within(child.exited, 'p10 hook fixture exit', 2_000);
    } catch {
      child.kill('SIGKILL');
      await child.exited;
    }
    void stderr;
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const ready = await nextFixtureMessage(messages, 'p10 hook fixture readiness');
  assert.equal(ready.type, 'ready');
  assert.equal(ready.port, port);
  assert.equal(
    ready.effectRoutesInstalled,
    true,
    'the fixture installed the Effect port — these legs run on the Effect park, not the legacy rollback',
  );

  board = await connectBoardClient(base, token);

  const send = async (command: 'counts' | 'close'): Promise<void> => {
    child.stdin.write(`${command}\n`);
    await child.stdin.flush();
  };

  return { base, port, token, board, messages, send };
}

// Register a session and park a raw hook hold; return the parked socket once the
// fixture confirms runHeldCalls===1 (Effect park taken) and the response is
// tracked. Shared by the shutdown / disconnect legs.
async function parkHookHold(fx: BootedFixture, sid: string): Promise<HookSocket> {
  const started = await hook(fx.base, fx.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');
  const held = await openHookSocket(fx.port, fx.token, sid);
  await pendingPermission(fx.base, fx.token, sid, `${sid} parked permission`);

  await fx.send('counts');
  const parked = await nextFixtureMessage(fx.messages, `${sid} parked counts`);
  assert.equal(parked.type, 'counts');
  assert.equal(
    parked.runHeldCalls,
    1,
    'the parked hook armed the Effect held primitive (holdHook effectRoutes branch -> settleEffectHookHold -> runHeld); a legacy-park hook would leave this 0',
  );
  assert.equal(
    parked.ownedCounts.activeResponses,
    1,
    'the parked hook is a tracked (hook) response',
  );
  assert.equal(parked.ownedCounts.watchWaiters, 0, 'a hook hold registers no watch closer');
  assert.equal((held as HookSocket).received(), 0, 'a parked hook has written no bytes yet');
  return held;
}

async function waitRunHeldCalls(
  fx: BootedFixture,
  n: number,
  label: string,
): Promise<FixtureCounts> {
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    await fx.send('counts');
    const msg = await nextFixtureMessage(fx.messages, label);
    if (msg.type === 'counts' && msg.runHeldCalls === n) return msg;
    await Bun.sleep(25);
  }
  throw new Error(`timed out waiting for ${label} (runHeldCalls === ${n})`);
}

// ============================ EFFECT-PARK LEG PINS ============================

test('P10 slice3 answer leg: a board answer settles the parked hook through the Effect Deferred with the decision wire (the ONLY body-carrying leg)', async (t) => {
  const fx = await bootFixture(t);
  const sid = 'p10-hook-answer';

  const started = await hook(fx.base, fx.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');

  // Fire the held hook but do NOT await it — it parks until the board answers.
  const held = hook(fx.base, fx.token, {
    hook_event_name: 'PermissionRequest',
    session_id: sid,
    cwd: '/tmp',
    tool_name: 'Bash',
    tool_input: { command: 'rm -rf build/' },
  });

  const question = await pendingPermission(fx.base, fx.token, sid, 'answer-leg parked permission');

  await fx.send('counts');
  const parked = await nextFixtureMessage(fx.messages, 'answer-leg parked counts');
  assert.equal(parked.type, 'counts');
  assert.equal(
    parked.runHeldCalls,
    1,
    'the parked hook armed the Effect held primitive; a legacy-park hook would leave runHeldCalls 0',
  );
  assert.equal(parked.ownedCounts.activeResponses, 1, 'the parked hook is a tracked response');

  const answered = await postJson(fx.base, `/api/questions/${question.id}/answer`, fx.token, {
    behavior: 'allow',
  });
  assert.equal(answered.status, 200, 'the board answer POST succeeds');

  const settled = await within(held, 'answer-leg held response');
  assert.equal(settled.status, 200, 'an answered hook always renders 200');
  assert.deepEqual(
    settled.body,
    { hookSpecificOutput: { hookEventName: 'PermissionRequest', decision: { behavior: 'allow' } } },
    'the answer leg carries the board decision body through the Effect settle (settle({body:obj}) -> settleEffectHookHold json .body); this is the one leg that is not fail-open {}',
  );

  // Shutdown still drains cleanly with nothing left parked.
  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'answer-leg lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.equal(
    (closed as FixtureClosed).runHeldCalls,
    1,
    'no second held primitive was armed — the answer settled the one parked hook',
  );
  assert.deepEqual((closed as FixtureClosed).ownedCounts, ZERO_OWNED_COUNTS);
});

test('P10 slice3 shutdown leg (D2): releaseAll settles the parked hook to 200 {} while the transport is still writable', async (t) => {
  const fx = await bootFixture(t);
  const held = await parkHookHold(fx, 'p10-hook-shutdown');

  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'shutdown-leg lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.equal(
    (closed as FixtureClosed).runHeldCalls,
    1,
    'the shutdown settled the SAME Effect hold; no legacy park ran',
  );
  assert.deepEqual(
    (closed as FixtureClosed).ownedCounts,
    ZERO_OWNED_COUNTS,
    'shutdown settles the hook and releases every owned resource',
  );

  const settled = await within(held.response, 'shutdown-leg fail-open body');
  assert.equal(settled.status, 200, 'a hook released on shutdown always answers 200');
  assert.deepEqual(
    settled.body,
    {},
    'D2 shutdown fails the hook open — releaseAll respond({}) -> settle({body:{}}) -> settleEffectHookHold json {} (NOT the watch idle body)',
  );

  held.socket.destroy();
});

test('P10 slice3 socket-disconnect leg (1A.3): a client abort writes NOTHING, never re-arms, and leaves runHeldCalls at 1', async (t) => {
  const fx = await bootFixture(t);
  const sid = 'p10-hook-disconnect';
  const held = await parkHookHold(fx, sid);

  // The peer vanishes before any settle leg fires. res.on('close') calls
  // socketClosed(id) (releases the manager's hold WITHOUT a respond and suppresses
  // re-arm — 1A.3 / §6-Q5) and abandon() (resolves the Deferred {_tag:'abandon'}),
  // so settleEffectHookHold writes NOTHING.
  held.socket.destroy();
  await Bun.sleep(150);

  await fx.send('counts');
  const afterClose = await nextFixtureMessage(fx.messages, 'disconnect-leg post-abort counts');
  assert.equal(afterClose.type, 'counts');
  assert.equal(
    (afterClose as FixtureCounts).runHeldCalls,
    1,
    'the abandon resolved the SAME Effect hold — no legacy park ran as a fallback',
  );
  assert.equal(held.received(), 0, 'the socket-close leg wrote nothing to the parked hook');

  // No re-arm: socketClosed released the hold without minting a successor card.
  const remaining = await stateQuestions(fx.base, fx.token);
  assert.equal(
    remaining.filter((q) => q.session_id === sid && q.status === 'pending').length,
    0,
    'socket disconnect suppresses re-arm — no pending successor question',
  );

  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'disconnect-leg lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.deepEqual(
    (closed as FixtureClosed).ownedCounts,
    ZERO_OWNED_COUNTS,
    'shutdown clears the abandoned hook response and every owned resource',
  );
});

test('P10 slice3 board-disconnect leg (1A.5): the last snapshot client 1->0 fails the parked hook open to 200 {} through the Effect settle', async (t) => {
  const fx = await bootFixture(t);
  const held = await parkHookHold(fx, 'p10-hook-board-disconnect');

  // The last authorized board closes: snapshotClients 1->0 fires failOpenAllHolds
  // (responder-first), settling every parked hold to the canonical {}.
  await closeBoardClient(fx.board);

  const settled = await within(held.response, 'board-disconnect fail-open body');
  assert.equal(settled.status, 200, 'a board-disconnect release always answers 200');
  assert.deepEqual(
    settled.body,
    {},
    'board disconnect fails the hook open — failOpenAllHolds respond({}) -> settle({body:{}}) -> 200 {}',
  );

  await fx.send('counts');
  const afterRelease = await nextFixtureMessage(
    fx.messages,
    'board-disconnect post-release counts',
  );
  assert.equal(afterRelease.type, 'counts');
  assert.equal(
    (afterRelease as FixtureCounts).runHeldCalls,
    1,
    'the board disconnect settled the SAME Effect hold; no legacy park ran',
  );

  held.socket.destroy();
});

test('P10 slice3 floor anti-truncation: a parked Effect hold is still open after HOOK_REPLY_FLOOR_MS (the hold settler never arms that timer)', {
  timeout: 20_000,
}, async (t) => {
  const fx = await bootFixture(t);
  const held = await parkHookHold(fx, 'p10-hook-floor');

  // HOOK_REPLY_FLOOR_MS defaults to 5000 and lives ONLY on settleEffectHookRoute.
  // If someone copied the floor onto settleEffectHookHold, this park would
  // fail-open 200 {} at 5s and truncate a 600s permission. Wait past the floor
  // and prove the hold is still unsettled.
  await Bun.sleep(5_500);

  assert.equal(
    held.received(),
    0,
    'a parked Effect hold writes no bytes after HOOK_REPLY_FLOOR_MS — the hold settler never arms the 5s floor',
  );

  await fx.send('counts');
  const stillParked = await nextFixtureMessage(fx.messages, 'floor-pin still-parked counts');
  assert.equal(stillParked.type, 'counts');
  assert.equal(
    (stillParked as FixtureCounts).runHeldCalls,
    1,
    'the floor wait did not arm a second held primitive',
  );
  assert.equal(
    (stillParked as FixtureCounts).ownedCounts.activeResponses,
    1,
    'the parked hook is still a tracked response after the floor elapsed — not settled',
  );

  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'floor-pin lifecycle close');
  assert.equal(closed.type, 'closed');
  const settled = await within(held.response, 'floor-pin shutdown fail-open');
  assert.equal(settled.status, 200);
  assert.deepEqual(settled.body, {}, 'shutdown still fails the surviving hold open');
  held.socket.destroy();
});

test('P10 slice3 cap-eviction leg (1A.4): the 5th parked Effect hold on one session fails the OLDEST open as 200 {} and leaves the rest parked', async (t) => {
  const fx = await bootFixture(t);
  const sid = 'p10-hook-cap';
  const started = await hook(fx.base, fx.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');

  const helds: HookSocket[] = [];
  for (let i = 0; i < 5; i++) {
    helds.push(await openHookSocket(fx.port, fx.token, sid, `c${i}`));
    await waitRunHeldCalls(fx, i + 1, `cap-eviction park ${i + 1}`);
  }

  const [oldest, ...rest] = helds;
  assert.ok(oldest, 'the oldest hold was created');
  const settled = await within(oldest.response, 'cap-eviction oldest fail-open');
  assert.equal(settled.status, 200, 'the evicted oldest hold always answers 200');
  assert.deepEqual(
    settled.body,
    {},
    'MAX_HOLDS_PER_SESSION=4: attaching the 5th fails the oldest open through the Effect settle (settleExpired -> respondFailOpen -> settle({body:{}}) -> 200 {})',
  );
  rest.forEach((h, idx) => {
    assert.equal(
      h.received(),
      0,
      `hold #${idx + 2} stays parked (no bytes) after the cap eviction`,
    );
  });

  await fx.send('counts');
  const after = await nextFixtureMessage(fx.messages, 'cap-eviction post-evict counts');
  assert.equal(after.type, 'counts');
  assert.equal(
    (after as FixtureCounts).runHeldCalls,
    5,
    'all five holds armed the Effect park; the eviction settled the oldest of those five, it did not fall through to a legacy park',
  );
  assert.equal(
    (after as FixtureCounts).ownedCounts.activeResponses,
    4,
    'the evicted oldest is no longer a tracked response; the other four remain parked',
  );

  await fx.send('close');
  const closed = await nextFixtureMessage(fx.messages, 'cap-eviction lifecycle close');
  assert.equal(closed.type, 'closed');
  assert.deepEqual((closed as FixtureClosed).ownedCounts, ZERO_OWNED_COUNTS);
  for (const h of helds) h.socket.destroy();
});

test('P10 slice3 dismiss leg (design #7): board POST /api/questions/:id/dismiss fails the parked Effect hold open as 200 {}', async (t) => {
  // Design §1A #7 is dismiss / expireOnActivity / expireOrphans, NOT the
  // pre-park quiescing/null-row early return in holdHook. The fixture already
  // wires questionsDismissWorkflow, so dismiss is the cheap HTTP representative.
  // expireOrphans skips holds.has live sockets (no respond) — no parked-hold
  // HTTP arm to drive. expireOnActivity is the same respondFailOpen seam as
  // dismiss; one HTTP arm covers the manager-to-Effect fold.
  const fx = await bootFixture(t);
  const sid = 'p10-hook-dismiss';
  const held = await parkHookHold(fx, sid);
  const question = await pendingPermission(fx.base, fx.token, sid, 'dismiss-leg parked permission');

  const dismissed = await postJson(fx.base, `/api/questions/${question.id}/dismiss`, fx.token, {});
  assert.equal(dismissed.status, 200, 'the board dismiss POST succeeds');

  const settled = await within(held.response, 'dismiss-leg fail-open body');
  assert.equal(settled.status, 200, 'a dismissed hold always answers 200');
  assert.deepEqual(
    settled.body,
    {},
    'design #7 dismiss fails the hook open — questions.dismiss respondFailOpen -> settle({body:{}}) -> 200 {}',
  );

  await fx.send('counts');
  const after = await nextFixtureMessage(fx.messages, 'dismiss-leg post-dismiss counts');
  assert.equal(after.type, 'counts');
  assert.equal(
    (after as FixtureCounts).runHeldCalls,
    1,
    'dismiss settled the SAME Effect hold; no legacy park ran',
  );

  held.socket.destroy();
});

// ==================== IN-PROCESS ROLLBACK-SEAM BYTE PINS ====================
//
// The effect-park pins above prove the Effect settle answers correctly; these lock
// the OTHER side of the seam. Rollback is `effectRoutes` left null (holdHook's
// `if (effectRoutes && EFFECT_CORE_HOLD_RELAY)` branch is skipped, so the legacy
// imperative park answers). Two in-process boards — one WITHOUT installEffectRoutes
// (legacy park) and one WITH the full Effect port (effect park) — must answer the
// hook hold's terminals BYTE-IDENTICALLY, so the retained legacy park is a faithful
// rollback of the Effect path, not just frozen source. Both settle through the same
// json(res,200,…) helper, so content-type / nosniff / content-length are compared
// too, not just the body.

type LifecycleHandle = ReturnType<typeof createHttp>['lifecycle'];

const HOOK_PORT_BUILDERS = {
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
  hookHold: heldSettleWorkflow,
} as const;

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

// A full-response POST that collects headers + body, so byte-equivalence can
// compare the whole terminal (status/body/content-type/nosniff/content-length).
// Fires and returns a Promise that resolves ONLY when the parked hook settles.
function rawFullPost(
  port: number,
  reqPath: string,
  token: string,
  body: Record<string, unknown>,
): Promise<RawResponse> {
  const payload = JSON.stringify(body);
  return new Promise<RawResponse>((resolve, reject) => {
    const request = http.request(
      {
        host: '127.0.0.1',
        port,
        path: reqPath,
        method: 'POST',
        headers: {
          authorization: `Bearer ${token}`,
          connection: 'close',
          'content-type': 'application/json',
          'content-length': Buffer.byteLength(payload),
        },
      },
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
    request.setTimeout(8_000, () => request.destroy(new Error('raw hook request timed out')));
    request.once('error', reject);
    request.end(payload);
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
  board: WebSocket;
  lifecycle: LifecycleHandle;
}

// A real createHttp/createCore board in THIS process WITH one authorized snapshot
// client attached (so a hook parks). installRoutes=false leaves effectRoutes null
// (rollback park); installRoutes=true wires the full Effect port. FLEETDECK_HOLD_SCOPE
// must already be 'all' when this runs — events.ts captures the scope at createCore.
async function startInProcBoard(
  t: { after: (fn: () => void | Promise<void>) => void },
  installRoutes: boolean,
): Promise<InProcBoard> {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p10-hook-inproc-'));
  const db = openDb(path.join(home, 'fleetd.db'));
  const port = await freePort();
  const token = '0123456789abcdef0123456789abcdef';
  const base = `http://127.0.0.1:${port}`;
  const core = createCore(db, { port, home, holdMs: 30_000, version: '0.0.0-test' });
  const handle = createHttp(core, { port, token, version: '0.0.0-test' });
  if (installRoutes) {
    handle.installEffectRoutes({
      runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
      ...HOOK_PORT_BUILDERS,
    });
  }
  await new Promise<void>((resolve, reject) => {
    handle.server.once('error', reject);
    handle.server.listen(port, '127.0.0.1', resolve);
  });
  const board = await connectBoardClient(base, token);
  let closed = false;
  t.after(async () => {
    await closeBoardClient(board);
    if (!closed) {
      closed = true;
      await handle.lifecycle.close();
    }
    await core.lifecycle.close();
    db.close();
    rmSync(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { port, base, token, board, lifecycle: handle.lifecycle };
}

// Register a session, park a hook via rawFullPost, answer it, and capture the full
// settled terminal. The answer wire is session-independent, so both parks match.
async function captureAnswerWire(board: InProcBoard, sid: string): Promise<RawResponse> {
  const started = await hook(board.base, board.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');
  const held = rawFullPost(board.port, '/hook/PermissionRequest', board.token, {
    hook_event_name: 'PermissionRequest',
    session_id: sid,
    cwd: '/tmp',
    tool_name: 'Bash',
    tool_input: { command: 'rm -rf build/' },
  });
  const question = await pendingPermission(
    board.base,
    board.token,
    sid,
    `${sid} inproc permission`,
  );
  const answered = await postJson(board.base, `/api/questions/${question.id}/answer`, board.token, {
    behavior: 'allow',
  });
  assert.equal(answered.status, 200, 'the board answer POST succeeds');
  return within(held, `${sid} answer wire`);
}

// Register a session, park a hook, drop the last board, and capture the fail-open
// terminal (failOpenAllHolds -> {}).
async function captureBoardDisconnectFailOpen(
  board: InProcBoard,
  sid: string,
): Promise<RawResponse> {
  const started = await hook(board.base, board.token, {
    hook_event_name: 'SessionStart',
    session_id: sid,
    cwd: '/tmp',
  });
  assert.equal(started.status, 200, 'SessionStart registers a live session');
  const held = rawFullPost(board.port, '/hook/PermissionRequest', board.token, {
    hook_event_name: 'PermissionRequest',
    session_id: sid,
    cwd: '/tmp',
    tool_name: 'Bash',
    tool_input: { command: 'rm -rf build/' },
  });
  await pendingPermission(board.base, board.token, sid, `${sid} inproc fail-open permission`);
  await closeBoardClient(board.board);
  return within(held, `${sid} fail-open wire`);
}

// Set the hold scope for the two in-process cores created below. events.ts reads
// process.env at createCore time, so both boards must be built while it is 'all';
// once built, the captured closure is immune to the restore.
async function withHoldScopeAll<T>(fn: () => Promise<T>): Promise<T> {
  const prev = process.env['FLEETDECK_HOLD_SCOPE'];
  process.env['FLEETDECK_HOLD_SCOPE'] = 'all';
  try {
    return await fn();
  } finally {
    if (prev === undefined) delete process.env['FLEETDECK_HOLD_SCOPE'];
    else process.env['FLEETDECK_HOLD_SCOPE'] = prev;
  }
}

test('P10 slice3 rollback seam: effectRoutes=null (legacy park) and the installed Effect park emit BYTE-IDENTICAL answer-wire bytes', async (t) => {
  const [legacyBoard, effectBoard] = await withHoldScopeAll(async () => [
    await startInProcBoard(t, false),
    await startInProcBoard(t, true),
  ]);

  const legacyWire = await captureAnswerWire(legacyBoard, 'p10-inproc-legacy-answer');
  const effectWire = await captureAnswerWire(effectBoard, 'p10-inproc-effect-answer');

  assert.equal(legacyWire.status, 200, 'the legacy rollback park answers 200');
  assert.equal(
    legacyWire.body,
    '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}',
    'the answer leg writes the decision wire on the legacy park',
  );
  assertByteIdentical(
    effectWire,
    legacyWire,
    'hook answer wire: effect park == legacy rollback park',
  );
});

test('P10 slice3 rollback seam: effectRoutes=null (legacy park) and the installed Effect park emit BYTE-IDENTICAL board-disconnect fail-open {} bytes', async (t) => {
  const [legacyBoard, effectBoard] = await withHoldScopeAll(async () => [
    await startInProcBoard(t, false),
    await startInProcBoard(t, true),
  ]);

  const legacyFailOpen = await captureBoardDisconnectFailOpen(legacyBoard, 'p10-inproc-legacy-fo');
  const effectFailOpen = await captureBoardDisconnectFailOpen(effectBoard, 'p10-inproc-effect-fo');

  assert.equal(legacyFailOpen.status, 200, 'the legacy rollback park fails open 200');
  assert.equal(
    legacyFailOpen.body,
    '{}',
    'board disconnect writes the canonical {} on the legacy park',
  );
  assertByteIdentical(
    effectFailOpen,
    legacyFailOpen,
    'hook board-disconnect fail-open: effect park == legacy rollback park',
  );
});
