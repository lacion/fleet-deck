// http-workflow-settings-command-mail-cleanup.test.ts — the focused fixture for
// the P6.4 SETTINGS/COMMAND/MAIL/CLEANUP route group. It pins the CONVENTION
// established in src/daemon/app/http-workflows/settings-command-mail-cleanup.ts
// three ways:
//
//   A. ISOLATION — the workflow Effects in pure isolation, with capability
//      fakes, asserting ControlPayload mapping, lazy thunk resolution, the
//      mail adapter (bare delivery vs {status,body} refusal), cleanup 409, and
//      that 400/422/429/503-from-core are DATA (E = never), not Effect errors.
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires installEffectRoutes
//      to the live ingress bridge) answers the four POSTs with the frozen
//      bytes, and the gateway_* bearer gate still fires at the transport
//      BEFORE the workflow (401 without the bearer; 200 with it).
//   C. IN-PROCESS EQUIVALENCE — on an idle in-memory core, toggling the bridge
//      on the SAME createHttp handle proves the workflow path is byte-identical
//      to the legacy handler; a quiescing/interrupted ingress answers the
//      frozen 503 shutting-down body WITHOUT performing the write; a workflow
//      defect reproduces each route's frozen 500 dialect.
//
// EXCLUSIONS (not converted; recorded here so a later worker does not "fix"
// them in): GET /mail (mutating drain+lease) and GET /api/watch (long-poll).
//
// The wire contract is byte-for-byte (docs/v1/evidence/effect/p6-http-matrix.md).

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { mapEffectRouteExit } from '../../src/daemon/http-policy.ts';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import type { IngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import { makeIngressSupervisor } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import {
  armUnsupervisedWorkflow,
  controlAsyncWorkflow,
  controlSyncWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
  spawnRouteWorkflow,
} from '../../src/daemon/app/http-workflows/control.ts';
import { healthWorkflow, stateWorkflow } from '../../src/daemon/app/http-workflows/health-state.ts';
import { hookDispatchWorkflow } from '../../src/daemon/app/http-workflows/hooks.ts';
import { pasteImageWorkflow } from '../../src/daemon/app/http-workflows/paste.ts';
import {
  cleanupWorkflow,
  commandWorkflow,
  mailWorkflow,
  settingsWorkflow,
} from '../../src/daemon/app/http-workflows/settings-command-mail-cleanup.ts';

import { startDaemon } from '../helpers/daemon.ts';
import { postJson } from '../helpers/http.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

const SHUTDOWN_BODY = '{"ok":false,"reason":"shutting-down"}';
const BOARD_TOKEN = 'test-token';

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// focused fixture exercises only the settings/command/mail/cleanup routes; the
// other groups' builders are inert here but keep the install object well-typed.
const ALL_ROUTE_BUILDERS = {
  health: healthWorkflow,
  state: stateWorkflow,
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
  spawnRoute: spawnRouteWorkflow,
  hookDispatch: hookDispatchWorkflow,
};

// Isolation tests unwrap a successful Exit. The banned v3 leftover is the
// `Effect.runPromise` identifier; RC.110's runPromiseExit is the sanctioned runner.
async function runSuccess<A>(effect: Effect.Effect<A, never, never>): Promise<A> {
  const exit = await Effect.runPromiseExit(effect);
  assert.equal(Exit.isSuccess(exit), true, 'workflow must succeed (E = never)');
  if (!Exit.isSuccess(exit)) throw new Error('unreachable');
  return exit.value;
}

// ============================ A. ISOLATION ============================

test('settingsWorkflow relays setSettings status/body as ControlPayload DATA', () => {
  const body = { ok: false, reason: 'unknown setting "nope"' };
  let calls = 0;
  const effect = settingsWorkflow({
    setSettings: () => {
      calls += 1;
      return { status: 400, body };
    },
  });
  assert.equal(calls, 0, 'building the Effect must not write');
  const out = Effect.runSync(effect);
  assert.equal(calls, 1);
  assert.equal(out.status, 400);
  assert.equal(out.body, body);
  assert.deepEqual(Object.keys(out), ['status', 'body']);
});

test('settingsWorkflow 200 success is DATA (E = never), not an Effect success-vs-fail split', () => {
  const body = { ok: true, settings: { hold_ms: 1000 } };
  const out = Effect.runSync(
    settingsWorkflow({
      setSettings: () => ({ status: 200, body }),
    }),
  );
  assert.equal(out.status, 200);
  assert.equal(out.body, body);
});

test('commandWorkflow is unconditional 200 of the command() result', () => {
  const marker = { ok: true, parsed: { cmd: 'note', text: 'hi' }, delivered: 0 };
  let calls = 0;
  const effect = commandWorkflow({
    command: () => {
      calls += 1;
      return marker;
    },
  });
  assert.equal(calls, 0);
  const out = Effect.runSync(effect);
  assert.equal(calls, 1);
  assert.equal(out.status, 200);
  assert.equal(out.body, marker);
});

test('mailWorkflow adapts a bare delivery object to 200 and the object as body', async () => {
  const delivery = { ok: true, delivered: 0, targets: [] as const };
  let calls = 0;
  const effect = mailWorkflow({
    postMail: () => {
      calls += 1;
      return Promise.resolve(delivery);
    },
  });
  assert.equal(calls, 0, 'Effect.promise factory runs when the Effect runs, not when built');
  const out = await runSuccess(effect);
  assert.equal(calls, 1);
  assert.equal(out.status, 200);
  assert.equal(out.body, delivery);
});

test('mailWorkflow adapts a {status, body} refusal (422/429/503-from-core) as DATA', async () => {
  const lifecycle = {
    status: 503,
    body: { ok: false, reason: 'mail lifecycle is quiescing' },
  };
  const out = await runSuccess(mailWorkflow({ postMail: () => Promise.resolve(lifecycle) }));
  assert.equal(out.status, 503);
  assert.equal(out.body, lifecycle.body);
  assert.notEqual(
    JSON.stringify(out.body),
    SHUTDOWN_BODY,
    'core-mail quiesce is NOT the ingress shutting-down body',
  );

  const unprocessable = {
    status: 422,
    body: { ok: false, reason: "sender name 'human' is reserved for the daemon" },
  };
  const out422 = await runSuccess(mailWorkflow({ postMail: () => Promise.resolve(unprocessable) }));
  assert.equal(out422.status, 422);
  assert.equal(out422.body, unprocessable.body);
});

test('cleanupWorkflow maps ok:true → 200 and ok:false → 409, body verbatim', async () => {
  const ok = {
    ok: true as const,
    archived: 0,
    mail_expired: 0,
    questions_expired: 0,
    questions_purged: 0,
    conflicts_cleared: 0,
    feed_cleared: 0,
    windows_killed: 0,
    orphan_worktrees: [] as const,
  };
  const success = await runSuccess(cleanupWorkflow({ cleanup: () => Promise.resolve(ok) }));
  assert.equal(success.status, 200);
  assert.equal(success.body, ok);

  const blocked = { ok: false as const, reason: 'tmux window listing unavailable' };
  const conflict = await runSuccess(cleanupWorkflow({ cleanup: () => Promise.resolve(blocked) }));
  assert.equal(conflict.status, 409);
  assert.equal(conflict.body, blocked);
});

test('mapEffectRouteExit still classifies interrupts-only as quiesce (this group maps that to 503)', () => {
  assert.deepEqual(mapEffectRouteExit(Exit.failCause(Cause.interrupt(1))), { kind: 'quiesce' });
  const quiescing = new ApplicationQuiescingError({
    operation: 'POST /api/settings',
    message: 'quiescing',
  });
  assert.deepEqual(mapEffectRouteExit(Exit.fail(quiescing)), { kind: 'quiesce' });
});

// ============================ B. REAL DAEMON WIRE ============================

test('the wired daemon answers the four POSTs with the frozen bytes; gateway gate stays at transport', async () => {
  const daemon = await startDaemon({ env: { FLEETDECK_TRUST_LOOPBACK: 'off' } });
  try {
    const base = daemon.baseUrl;
    const token = daemon.token;

    // Gap 4: gateway_* without the bearer is refused at the transport — the
    // workflow must not run (a 200 here would mean the gate moved inside).
    const gated = await postJson(`${base}/api/settings`, {
      gateway_base_url: 'https://gw.example.com',
    });
    assert.equal(gated.status, 401);
    assert.deepEqual(gated.json, {
      ok: false,
      reason: 'gateway settings require the bearer token',
    });

    const authed = await postJson(
      `${base}/api/settings`,
      { gateway_base_url: 'https://gw.example.com' },
      { token },
    );
    assert.equal(authed.status, 200);
    assert.equal((authed.json as { ok?: unknown }).ok, true);

    // Plain (non-gateway) settings stay loopback-open.
    const plain = await postJson(`${base}/api/settings`, { repo_transport: 'https' });
    assert.equal(plain.status, 200);
    assert.equal((plain.json as { ok?: unknown }).ok, true);
    assert.equal(
      (await fetch(`${base}/api/settings`, { method: 'POST', body: '{}' })).status,
      415,
      'content-type wall stays at transport',
    );

    const unknown = await postJson(`${base}/api/settings`, { nope: 1 });
    assert.equal(unknown.status, 400);
    assert.equal((unknown.json as { ok?: unknown }).ok, false);

    const command = await postJson(`${base}/command`, { text: 'p6.4 settings-group note' });
    assert.equal(command.status, 200);
    const commandBody = command.json as { ok?: unknown; delivered?: unknown; parsed?: unknown };
    assert.equal(commandBody.ok, true);
    assert.equal(commandBody.delivered, 0);
    assert.equal(typeof commandBody.parsed, 'object');

    const noText = await postJson(`${base}/command`, {});
    assert.equal(noText.status, 200);

    const mailBare = await postJson(`${base}/mail`, {
      to: 'all',
      from: 'ops',
      text: 'p6.4',
    });
    assert.equal(mailBare.status, 401, 'POST /mail is token-gated even on loopback');

    const mail = await postJson(
      `${base}/mail`,
      { to: 'all', from: 'ops', text: 'p6.4' },
      { token },
    );
    assert.equal(mail.status, 200);
    const mailBody = mail.json as { ok?: unknown; delivered?: unknown; targets?: unknown };
    assert.equal(mailBody.ok, true);
    assert.equal(mailBody.delivered, 0);
    assert.ok(Array.isArray(mailBody.targets));

    const reserved = await postJson(
      `${base}/mail`,
      { to: 'all', from: 'human', text: 'nope' },
      { token },
    );
    assert.equal(reserved.status, 422);
    assert.equal((reserved.json as { ok?: unknown }).ok, false);

    const cleanup = await postJson(`${base}/api/cleanup`, {});
    assert.equal(cleanup.status, 200);
    assert.equal((cleanup.json as { ok?: unknown }).ok, true);
    const cleanupKeys = Object.keys(cleanup.json as Record<string, unknown>).sort();
    assert.deepEqual(cleanupKeys, [
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
  } finally {
    await daemon.stop();
  }
  assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
});

// ============================ C. IN-PROCESS EQUIVALENCE ============================

type BoardHandle = ReturnType<typeof createHttp> & { port: number };

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

function rawFull(
  port: number,
  {
    method = 'GET',
    path: reqPath = '/',
    headers = {},
    body = null,
  }: {
    method?: string;
    path?: string;
    headers?: Record<string, string>;
    body?: string | null;
  } = {},
): Promise<RawResponse> {
  return new Promise<RawResponse>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, (res) => {
      let text = '';
      res.on('data', (d: Buffer) => {
        text += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, headers: res.headers, body: text });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    req.end(body);
  });
}

function postRaw(
  port: number,
  reqPath: string,
  obj: unknown,
  extraHeaders: Record<string, string> = {},
): Promise<RawResponse> {
  const body = JSON.stringify(obj);
  return rawFull(port, {
    method: 'POST',
    path: reqPath,
    headers: {
      'content-type': 'application/json',
      'content-length': String(Buffer.byteLength(body)),
      ...extraHeaders,
    },
    body,
  });
}

function startBoard(t: TestContext): Promise<BoardHandle> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  const probe = http.createServer();
  return new Promise<BoardHandle>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const handle = createHttp(core, { port, token: BOARD_TOKEN, lan: null });
        handle.server.once('error', reject);
        handle.server.listen(port, '127.0.0.1', () => {
          t.after(() => {
            handle.server.close();
            db.close();
          });
          resolve({ ...handle, port });
        });
      });
    });
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

function installSuccess(board: BoardHandle): void {
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
}

function installQuiesce(board: BoardHandle): void {
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });
}

function installInterrupt(board: BoardHandle): void {
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });
}

function installDefect(board: BoardHandle): void {
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(new Error('boom'))),
    ...ALL_ROUTE_BUILDERS,
  });
}

function assertShutdown503(res: RawResponse, label: string): void {
  assert.equal(res.status, 503, `${label}: status`);
  assert.equal(res.body, SHUTDOWN_BODY, `${label}: body`);
  assert.equal(res.headers['content-type'], 'application/json', `${label}: content-type`);
  assert.equal(res.headers['x-content-type-options'], 'nosniff', `${label}: nosniff`);
}

test('workflow dispatch is byte-identical to the legacy handler for the four POSTs', async (t) => {
  // Two idle boards: mutating routes change state, so sequential captures on
  // ONE handle would compare different feed/mail/settings histories, not paths.
  const legacyBoard = await startBoard(t);
  const workflowBoard = await startBoard(t);
  installSuccess(workflowBoard);
  const mailHeaders = { authorization: `Bearer ${BOARD_TOKEN}` };

  const pair = async (
    label: string,
    reqPath: string,
    obj: unknown,
    headers: Record<string, string> = {},
  ): Promise<void> => {
    const expected = await postRaw(legacyBoard.port, reqPath, obj, headers);
    const actual = await postRaw(workflowBoard.port, reqPath, obj, headers);
    assertByteIdentical(actual, expected, label);
  };

  await pair('POST /api/settings 400', '/api/settings', { nope: 1 });
  await pair('POST /api/settings 200', '/api/settings', { repo_transport: 'https' });
  await pair('POST /command', '/command', { text: 'p6.4 settings-group note' });
  await pair('POST /mail', '/mail', { to: 'all', from: 'ops', text: 'p6.4' }, mailHeaders);
  await pair('POST /api/cleanup', '/api/cleanup', {});
});

test('a quiescing ingress refuses with shutting-down 503 and does not perform the write', async (t) => {
  const board = await startBoard(t);
  const before = await rawFull(board.port, { path: '/api/settings' });
  installQuiesce(board);

  assertShutdown503(
    await postRaw(board.port, '/api/settings', { browse_root: '/should-not-apply' }),
    'POST /api/settings',
  );
  const after = await rawFull(board.port, { path: '/api/settings' });
  assert.equal(after.body, before.body, 'browse_root must not have been written during quiesce');

  assertShutdown503(
    await postRaw(board.port, '/command', { text: 'must-not-log' }),
    'POST /command',
  );
  assertShutdown503(
    await postRaw(
      board.port,
      '/mail',
      { to: 'all', from: 'ops', text: 'must-not-insert' },
      { authorization: `Bearer ${BOARD_TOKEN}` },
    ),
    'POST /mail',
  );
  assertShutdown503(await postRaw(board.port, '/api/cleanup', {}), 'POST /api/cleanup');
});

test('an interrupts-only Exit maps to the same 503 shutting-down refusal', async (t) => {
  const board = await startBoard(t);
  const before = await rawFull(board.port, { path: '/api/settings' });
  installInterrupt(board);

  assertShutdown503(
    await postRaw(board.port, '/api/settings', { browse_root: '/should-not-apply' }),
    'POST /api/settings interrupt',
  );
  const after = await rawFull(board.port, { path: '/api/settings' });
  assert.equal(after.body, before.body, 'interrupt must not replay the write');
  assertShutdown503(
    await postRaw(board.port, '/command', { text: 'must-not-log' }),
    'POST /command interrupt',
  );
});

test('a workflow defect reproduces each route frozen 500 dialect', async (t) => {
  const board = await startBoard(t);
  installDefect(board);

  const settings = await postRaw(board.port, '/api/settings', { repo_transport: 'https' });
  assert.equal(settings.status, 500);
  assert.equal(settings.body, '{"err":"internal"}');
  assert.equal(settings.headers['content-type'], 'application/json');
  assert.equal(settings.headers['x-content-type-options'], 'nosniff');

  const command = await postRaw(board.port, '/command', { text: 'x' });
  assert.equal(command.status, 500);
  assert.equal(command.body, '{"err":"internal"}');

  const mail = await postRaw(
    board.port,
    '/mail',
    { to: 'all', from: 'ops', text: 'x' },
    { authorization: `Bearer ${BOARD_TOKEN}` },
  );
  assert.equal(mail.status, 500);
  assert.equal(mail.body, '{"ok":false,"err":"internal"}');

  const cleanup = await postRaw(board.port, '/api/cleanup', {});
  assert.equal(cleanup.status, 500);
  assert.equal(cleanup.body, '{"ok":false,"err":"internal"}');
});

test('gateway_* 401 still fires at the transport when the Effect bridge is wired', async (t) => {
  const board = await startBoard(t);
  installSuccess(board);
  const gated = await postRaw(board.port, '/api/settings', {
    gateway_base_url: 'https://gw.example.com',
  });
  assert.equal(gated.status, 401);
  assert.equal(gated.body, '{"ok":false,"reason":"gateway settings require the bearer token"}');
});

test('effectRoutes=null is the rollback seam: the four POSTs stay on the legacy path', async (t) => {
  const board = await startBoard(t);
  const legacy = await postRaw(board.port, '/api/settings', { nope: 1 });
  assert.equal(legacy.status, 400);
  // Never installed — still the legacy handler. A second capture must match.
  const again = await postRaw(board.port, '/api/settings', { nope: 1 });
  assertByteIdentical(again, legacy, 'rollback seam');
});

// =================== D. LIVE INGRESS BRIDGE (JOIN-ON-INTERRUPT) ===================
// A, B and C stub runRequest with fixed Exits (installQuiesce/installInterrupt/
// installDefect) or run the workflow off the interruption path. A fixed Exit can
// PROVE the settler's branch table, but it cannot reproduce the one behaviour this
// fix exists to guarantee: that when the shutdown fiber interrupt()s an ALREADY-
// admitted async mutation whose native core Promise settles only AFTER the
// interrupt fires, the response still answers that Promise's TRUE result (never
// 503/500) and JOINS its completion before resolving — so http.closeClients (which
// awaits res.done) waits for the write exactly as the legacy .then(json) chain did.
// These tests therefore wire the REAL LiveIngressSupervisor as runRequest (the same
// object program.ts installs) and drive an actual interrupt() against a gated core
// method. A separate quiesce-before-admission assertion pins the ONLY 503 case: a
// refusal that never invokes core at all (invocation counter === 0).

// The sanctioned Effect runner: Context.empty() ⇒ Services = never, so the built
// supervisor's runPromiseExit accepts exactly the R = never HttpWorkflowEffect the
// bridge submits (the import-boundaries tripwire bans bare Effect.runPromise/Fork,
// not runPromiseWith / the runPromiseExit method).
const runIngress = Effect.runPromiseWith(Context.empty());

interface Gate {
  /** How many times the real core method was actually invoked (0 ⇒ a true refusal). */
  invocations(): number;
  /** Whether the native operation has settled (flips strictly before the settler joins it). */
  settled(): boolean;
  /** Let the gated native operation run to completion. */
  release(): void;
}

// Replace ONE async core method with a gate: its returned Promise settles only
// after release(), modelling core.postMail / core.cleanup still in flight when the
// shutdown fiber interrupts the request. Capabilities read core.<method> at invoke
// time, so overriding it after createHttp still takes effect. The recorder in
// http.ts captures THIS exact Promise, so `settled` flipping before the response
// resolves is a direct witness that the settler joined the native op, not a 503.
function gateAsyncMethod(core: unknown, name: string): Gate {
  const holder = core as Record<string, (...args: unknown[]) => Promise<unknown>>;
  const original = (holder[name] as (...args: unknown[]) => Promise<unknown>).bind(holder);
  let invocations = 0;
  let settled = false;
  let open: () => void = () => undefined;
  const opened = new Promise<void>((resolve) => {
    open = resolve;
  });
  holder[name] = (...args: unknown[]): Promise<unknown> => {
    invocations += 1;
    return opened
      .then(() => original(...args))
      .then(
        (out) => {
          settled = true;
          return out;
        },
        (err) => {
          settled = true;
          throw err;
        },
      );
  };
  return {
    invocations: () => invocations,
    settled: () => settled,
    release: () => open(),
  };
}

interface LiveBoard {
  readonly port: number;
  readonly core: ReturnType<typeof createCore>;
  readonly supervisor: IngressSupervisorService<never>;
}

// Same in-memory board as startBoard, but the bridge is the REAL supervisor
// (runRequest === supervisor.runPromiseExit, exactly as makeHttpServerOwner wires
// it in production), so interrupt()/quiesce() drive genuine fiber lifecycle; `core`
// is handed back so a test can gate one method before firing a request.
function startLiveBoard(t: TestContext): Promise<LiveBoard> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  const rootScope = Scope.makeUnsafe('sequential');
  const probe = http.createServer();
  return new Promise<LiveBoard>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        runIngress(makeIngressSupervisor(Context.empty(), rootScope)).then((supervisor) => {
          const handle = createHttp(core, { port, token: BOARD_TOKEN, lan: null });
          handle.installEffectRoutes({
            runRequest: (operation, effect) => supervisor.runPromiseExit(operation, effect),
            ...ALL_ROUTE_BUILDERS,
          });
          handle.server.once('error', reject);
          handle.server.listen(port, '127.0.0.1', () => {
            t.after(async () => {
              handle.server.close();
              db.close();
              await runIngress(Scope.close(rootScope, Exit.void));
            });
            resolve({ port, core, supervisor });
          });
        }, reject);
      });
    });
  });
}

async function waitFor(pred: () => boolean, label: string, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (!pred()) {
    if (Date.now() - start > timeoutMs)
      throw new Error(`${label} not reached within ${timeoutMs}ms`);
    await Bun.sleep(1);
  }
}

async function within<A>(promise: Promise<A>, label: string, timeoutMs = 3000): Promise<A> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(
          () => reject(new Error(`${label} did not settle within ${timeoutMs}ms`)),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

test('live bridge: interrupting an in-flight POST /mail joins the started write and answers its TRUE result, while a quiesce-before-start refuses with 503 and never calls postMail', async (t) => {
  // (a) + (b): admitted, in flight, then interrupted → JOIN, then true 200 result.
  const board = await startLiveBoard(t);
  const gate = gateAsyncMethod(board.core, 'postMail');
  const mailHeaders = { authorization: `Bearer ${BOARD_TOKEN}` };

  const reqP = postRaw(board.port, '/mail', { to: 'all', from: 'ops', text: 'live' }, mailHeaders);
  let responded = false;
  void reqP.then(() => {
    responded = true;
  });

  await waitFor(() => gate.invocations() === 1, 'postMail invoked');
  assert.equal(board.supervisor.activeCount, 1, 'the request fiber is in flight');
  assert.equal(gate.settled(), false, 'the native write has not settled yet');

  board.supervisor.interrupt();
  assert.equal(board.supervisor.state, 'quiescing', 'interrupt() quiesces admission');

  // The interrupt must NOT collapse to 503: the write already started, so the
  // settler JOINs it. The fiber's Exit resolves (activeCount → 0) but the response
  // stays pending on the still-gated native Promise.
  await waitFor(() => board.supervisor.activeCount === 0, 'the interrupted fiber settled');
  await Bun.sleep(20);
  assert.equal(responded, false, 'response must not resolve to 503 — it joins the started write');
  assert.equal(gate.settled(), false, 'the joined write is still gated');

  gate.release();
  const res = await within(reqP, 'joined POST /mail response');
  assert.equal(gate.settled(), true, 'the native write settled before the response resolved');
  assert.equal(res.status, 200, 'the TRUE postMail result — not 503, not 500');
  assert.equal((JSON.parse(res.body) as { ok?: unknown }).ok, true);
  assert.notEqual(res.body, SHUTDOWN_BODY, 'not the shutting-down refusal');
  assert.notEqual(res.body, '{"ok":false,"err":"internal"}', 'not the mail defect body');

  // (c) A quiesce BEFORE admission is the ONLY 503 case — postMail is never called.
  const refusalBoard = await startLiveBoard(t);
  const refusalGate = gateAsyncMethod(refusalBoard.core, 'postMail');
  refusalBoard.supervisor.quiesce();
  const refused = await within(
    postRaw(refusalBoard.port, '/mail', { to: 'all', from: 'ops', text: 'nope' }, mailHeaders),
    'quiesce-before-start POST /mail',
  );
  assertShutdown503(refused, 'POST /mail quiesce-before-start');
  assert.equal(refusalGate.invocations(), 0, 'a refused admission never invokes postMail');
});

test('live bridge: interrupting an in-flight POST /api/cleanup joins the started cleanup and answers its TRUE result, while a quiesce-before-start refuses with 503 and never calls cleanup', async (t) => {
  const board = await startLiveBoard(t);
  const gate = gateAsyncMethod(board.core, 'cleanup');

  const reqP = postRaw(board.port, '/api/cleanup', {});
  let responded = false;
  void reqP.then(() => {
    responded = true;
  });

  await waitFor(() => gate.invocations() === 1, 'cleanup invoked');
  assert.equal(board.supervisor.activeCount, 1, 'the request fiber is in flight');
  assert.equal(gate.settled(), false, 'the native cleanup has not settled yet');

  board.supervisor.interrupt();
  await waitFor(() => board.supervisor.activeCount === 0, 'the interrupted fiber settled');
  await Bun.sleep(20);
  assert.equal(responded, false, 'response must join the started cleanup, not resolve to 503');
  assert.equal(gate.settled(), false, 'the joined cleanup is still gated');

  gate.release();
  const res = await within(reqP, 'joined POST /api/cleanup response');
  assert.equal(gate.settled(), true, 'the cleanup settled before the response resolved');
  assert.equal(res.status, 200, 'the TRUE cleanup result — not 503, not 500');
  assert.equal((JSON.parse(res.body) as { ok?: unknown }).ok, true);
  assert.notEqual(res.body, SHUTDOWN_BODY, 'not the shutting-down refusal');
  assert.notEqual(res.body, '{"ok":false,"err":"internal"}', 'not the cleanup defect body');

  const refusalBoard = await startLiveBoard(t);
  const refusalGate = gateAsyncMethod(refusalBoard.core, 'cleanup');
  refusalBoard.supervisor.quiesce();
  const refused = await within(
    postRaw(refusalBoard.port, '/api/cleanup', {}),
    'quiesce-before-start POST /api/cleanup',
  );
  assertShutdown503(refused, 'POST /api/cleanup quiesce-before-start');
  assert.equal(refusalGate.invocations(), 0, 'a refused admission never invokes cleanup');
});
