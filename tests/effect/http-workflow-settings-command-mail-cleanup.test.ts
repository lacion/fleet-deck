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
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { mapEffectRouteExit } from '../../src/daemon/http-policy.ts';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import {
  controlAsyncWorkflow,
  controlSyncWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
} from '../../src/daemon/app/http-workflows/control.ts';
import { healthWorkflow, stateWorkflow } from '../../src/daemon/app/http-workflows/health-state.ts';
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
