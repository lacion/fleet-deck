// http-workflow-paste.test.ts — the focused fixture for the P6.4 paste-image
// route group (POST /api/paste-image). Static shell (GET /, /index.html,
// /assets/*) is intentionally legacy until P13 — see http-workflows/paste.ts.
// Pins the CONVENTION three ways, matching http-workflow-health-state.test.ts:
//
//   A. ISOLATION — the workflow Effect with capability fakes, asserting the
//      envelope is returned verbatim, frozen key order, and lazy thunks.
//   B. REAL DAEMON WIRE — a real daemon answers POST /api/paste-image with the
//      frozen bytes: 201 + .png path shape, JSON header trio, 400 data responses.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge
//      on the SAME createHttp handle proves the workflow path matches the legacy
//      handler (modulo the UUID in `path`), that a quiescing/interrupted ingress
//      answers the frozen shutdown 503 WITHOUT writing a paste, and that a
//      workflow defect reproduces the POST inner-catch 500 {err:'internal'}.
//
// Body-cap / CSRF / content-type walls stay in the transport (http.ts) and are
// already pinned by tests/paste-image.test.ts; this file does not re-derive them.

import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';

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
import {
  type PasteImageCapabilities,
  pasteImageWorkflow,
} from '../../src/daemon/app/http-workflows/paste.ts';
import {
  cleanupWorkflow,
  commandWorkflow,
  mailWorkflow,
  settingsWorkflow,
} from '../../src/daemon/app/http-workflows/settings-command-mail-cleanup.ts';

import { startDaemon } from '../helpers/daemon.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

const PASTE_OK_KEY_ORDER = ['ok', 'path', 'bytes'] as const;
const PASTE_ERR_KEY_ORDER = ['ok', 'reason'] as const;

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// focused fixture exercises only POST /api/paste-image; the other groups'
// builders are inert here but keep the install object well-typed.
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

const PNG = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  Buffer.alloc(64),
]);
const b64 = (buf: Buffer): string => Buffer.from(buf).toString('base64');

const QUIESCE_BODY = '{"ok":false,"reason":"shutting-down"}';
const INNER_CATCH_BODY = '{"err":"internal"}';

// ============================ A. ISOLATION ============================

test('pasteImageWorkflow returns the paste-store envelope verbatim', () => {
  const envelope = {
    status: 201,
    body: { ok: true as const, path: '/tmp/fake/paste-x.png', bytes: 72 },
  };
  let calls = 0;
  const caps: PasteImageCapabilities = {
    pasteImage: () => {
      calls += 1;
      return envelope;
    },
  };
  const effect = pasteImageWorkflow(caps);
  assert.equal(calls, 0, 'building the Effect must not touch the thunk');
  const out = Effect.runSync(effect);
  assert.equal(out, envelope, 'same reference — no copy, no reshape');
  assert.equal(calls, 1);
  assert.deepEqual(Object.keys(out.body as object), [...PASTE_OK_KEY_ORDER]);
});

test('pasteImageWorkflow treats 400/413/500 data responses as success (E=never)', () => {
  const cases = [
    { status: 400, body: { ok: false, reason: 'missing image data' } },
    { status: 400, body: { ok: false, reason: 'not valid base64' } },
    { status: 400, body: { ok: false, reason: 'not a supported image (png, jpeg, gif, webp)' } },
    { status: 413, body: { ok: false, reason: 'image exceeds 10485760 bytes' } },
    { status: 500, body: { ok: false, reason: 'write failed' } },
  ];
  for (const envelope of cases) {
    const out = Effect.runSync(pasteImageWorkflow({ pasteImage: () => envelope }));
    assert.equal(out, envelope, `${envelope.status} ${JSON.stringify(envelope.body)}`);
    assert.deepEqual(Object.keys(out.body as object), [...PASTE_ERR_KEY_ORDER]);
  }
});

test('mapEffectRouteExit still classifies paste-shaped success, quiesce, and defect', () => {
  const value = { status: 201, body: { ok: true, path: '/x.png', bytes: 1 } };
  assert.deepEqual(mapEffectRouteExit(Exit.succeed(value)), { kind: 'success', value });

  const quiescing = new ApplicationQuiescingError({
    operation: 'POST /api/paste-image',
    message: 'quiescing',
  });
  assert.deepEqual(mapEffectRouteExit(Exit.fail(quiescing)), { kind: 'quiesce' });
  assert.deepEqual(mapEffectRouteExit(Exit.failCause(Cause.interrupt(1))), { kind: 'quiesce' });

  const boom = new Error('boom');
  const died = mapEffectRouteExit(Exit.die(boom));
  assert.equal(died.kind, 'defect');
  assert.equal(died.kind === 'defect' ? died.defect : null, boom);
});

// ============================ B. REAL DAEMON WIRE ============================

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

function rawRequest(
  port: number,
  {
    method = 'GET',
    path: reqPath = '/',
    headers = {},
    body,
  }: {
    method?: string;
    path?: string;
    headers?: Record<string, string>;
    body?: string;
  } = {},
): Promise<RawResponse> {
  return new Promise<RawResponse>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, (res) => {
      let out = '';
      res.on('data', (d: Buffer) => {
        out += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, headers: res.headers, body: out });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    if (body != null) req.end(body);
    else req.end();
  });
}

function pastePost(port: number, payload: unknown): Promise<RawResponse> {
  const body = JSON.stringify(payload);
  return rawRequest(port, {
    method: 'POST',
    path: '/api/paste-image',
    headers: {
      'content-type': 'application/json',
      'content-length': String(Buffer.byteLength(body)),
    },
    body,
  });
}

function assertJsonHeaders(res: RawResponse, label: string): void {
  assert.equal(res.headers['content-type'], 'application/json', `${label}: content-type`);
  assert.equal(res.headers['x-content-type-options'], 'nosniff', `${label}: nosniff`);
  const cl = res.headers['content-length'];
  if (cl !== undefined)
    assert.equal(cl, String(Buffer.byteLength(res.body)), `${label}: content-length`);
}

test('the wired daemon answers POST /api/paste-image with the frozen 201 .png shape', async () => {
  const daemon = await startDaemon();
  try {
    const ok = await pastePost(daemon.port, { data: b64(PNG) });
    assert.equal(ok.status, 201);
    assertJsonHeaders(ok, '201');
    const okBody = JSON.parse(ok.body) as { ok: unknown; path: unknown; bytes: unknown };
    assert.deepEqual(Object.keys(okBody), [...PASTE_OK_KEY_ORDER]);
    assert.equal(okBody.ok, true);
    assert.equal(typeof okBody.path, 'string');
    assert.ok(String(okBody.path).endsWith('.png'), `${okBody.path} must end .png`);
    assert.ok(
      String(okBody.path).startsWith(path.join(daemon.home, 'pastes')),
      `${okBody.path} must live under the daemon home pastes dir`,
    );
    assert.equal(okBody.bytes, PNG.length);

    const missing = await pastePost(daemon.port, {});
    assert.equal(missing.status, 400);
    assertJsonHeaders(missing, '400 missing');
    const missingBody = JSON.parse(missing.body) as { ok: unknown; reason: unknown };
    assert.deepEqual(Object.keys(missingBody), [...PASTE_ERR_KEY_ORDER]);
    assert.equal(missingBody.ok, false);
    assert.equal(missingBody.reason, 'missing image data');
  } finally {
    await daemon.stop();
  }
  assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
});

// ============================ C. IN-PROCESS EQUIVALENCE ============================

type BoardHandle = ReturnType<typeof createHttp> & { port: number; home: string };

function startBoard(t: TestContext): Promise<BoardHandle> {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fd-paste-wf-'));
  const home = path.join(scratch, 'home');
  mkdirSync(home);
  const prevHome = process.env['FLEETDECK_HOME'];
  process.env['FLEETDECK_HOME'] = home;
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home });
  let cleaned = false;
  const cleanup = () => {
    if (cleaned) return;
    cleaned = true;
    if (prevHome === undefined) delete process.env['FLEETDECK_HOME'];
    else process.env['FLEETDECK_HOME'] = prevHome;
    try {
      db.close();
    } catch {
      /* already closed */
    }
    rmSync(scratch, { recursive: true, force: true });
  };
  t.after(cleanup);
  const probe = http.createServer();
  return new Promise<BoardHandle>((resolve, reject) => {
    probe.once('error', (err) => {
      cleanup();
      reject(err);
    });
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const handle = createHttp(core, { port, token: null as unknown as string, lan: null });
        handle.server.once('error', (err) => {
          cleanup();
          reject(err);
        });
        handle.server.listen(port, '127.0.0.1', () => {
          t.after(() => {
            handle.server.close();
          });
          resolve({ ...handle, port, home });
        });
      });
    });
  });
}

function pasteFiles(home: string): string[] {
  try {
    return readdirSync(path.join(home, 'pastes')).filter((n) => !n.endsWith('.tmp'));
  } catch {
    return [];
  }
}

function normalizePasteBody(body: string): unknown {
  const parsed = JSON.parse(body) as Record<string, unknown>;
  if (parsed['ok'] === true && typeof parsed['path'] === 'string') {
    return { ...parsed, path: '<paste-path>' };
  }
  return parsed;
}

function installFaithful(board: BoardHandle): void {
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
}

test('workflow dispatch matches the legacy handler for 201 and 400 paste-image', async (t) => {
  const board = await startBoard(t);

  const legacyOk = await pastePost(board.port, { data: b64(PNG) });
  const legacyMissing = await pastePost(board.port, {});
  const legacyBadB64 = await pastePost(board.port, { data: '!!!not base64!!!' });
  assert.equal(legacyOk.status, 201);
  assert.equal(legacyMissing.status, 400);
  assert.equal(legacyBadB64.status, 400);

  installFaithful(board);

  const workflowOk = await pastePost(board.port, { data: b64(PNG) });
  const workflowMissing = await pastePost(board.port, {});
  const workflowBadB64 = await pastePost(board.port, { data: '!!!not base64!!!' });

  assert.equal(workflowOk.status, legacyOk.status, '201 status');
  assert.deepEqual(
    normalizePasteBody(workflowOk.body),
    normalizePasteBody(legacyOk.body),
    '201 body (modulo UUID path)',
  );
  assert.equal(workflowOk.headers['content-type'], legacyOk.headers['content-type']);
  assert.equal(
    workflowOk.headers['x-content-type-options'],
    legacyOk.headers['x-content-type-options'],
  );

  assert.equal(workflowMissing.body, legacyMissing.body, '400 missing: body');
  assert.equal(workflowMissing.status, legacyMissing.status);
  assert.equal(workflowBadB64.body, legacyBadB64.body, '400 bad b64: body');
  assert.equal(workflowBadB64.status, legacyBadB64.status);
});

test('a quiescing ingress refuses paste-image with the frozen 503 and writes nothing', async (t) => {
  const board = await startBoard(t);
  assert.deepEqual(pasteFiles(board.home), []);

  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const refused = await pastePost(board.port, { data: b64(PNG) });
  assert.equal(refused.status, 503);
  assert.equal(refused.body, QUIESCE_BODY);
  assertJsonHeaders(refused, 'quiesce 503');
  assert.deepEqual(pasteFiles(board.home), [], 'quiesce must not write a kept paste');
});

test('an interrupts-only Exit refuses paste-image with the frozen 503 and writes nothing', async (t) => {
  const interrupted = Exit.failCause(Cause.interrupt(1));
  assert.deepEqual(mapEffectRouteExit(interrupted), { kind: 'quiesce' });

  const board = await startBoard(t);
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });

  const refused = await pastePost(board.port, { data: b64(PNG) });
  assert.equal(refused.status, 503);
  assert.equal(refused.body, QUIESCE_BODY);
  assertJsonHeaders(refused, 'interrupt 503');
  assert.deepEqual(pasteFiles(board.home), [], 'interrupt must not write a kept paste');
});

test('a workflow defect reproduces the POST inner-catch 500 {err:internal}', async (t) => {
  const board = await startBoard(t);
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(new Error('boom'))),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await pastePost(board.port, { data: b64(PNG) });
  assert.equal(defected.status, 500);
  assert.equal(defected.body, INNER_CATCH_BODY);
  assertJsonHeaders(defected, 'defect 500');
  assert.deepEqual(pasteFiles(board.home), [], 'a defect must not write a kept paste');
});
