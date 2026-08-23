// http-workflow-control.test.ts — the focused fixture for the P6.4 CONTROL route
// group: the 11 mutating board-action POSTs (POST /api/spawn/:id/{kill,revive,rc},
// /api/sessions/:sid/{adopt,dismiss,dismiss/retry,name}, /api/questions/:id/
// {answer,dismiss}, /api/plans/:id/{mark,assign}). It pins the CONVENTION in
// src/daemon/app/http-workflows/control.ts three ways, exactly as the pilot suite
// (http-workflow-health-state.test.ts) does for /health and /state:
//
//   A. ISOLATION — the four control workflows and the Exit → Response mapper in
//      pure isolation, with capability fakes: the exact (status, body) wire each
//      derive result maps to, the two 500 shapes a legacy handler emits (a promise
//      rejection folded into the success wire vs. a synchronous throw surfacing as
//      a die), lazy construction, and the name-suffix validation branches.
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires the control builders to
//      the live ingress bridge) answers representative control routes of every
//      shape with the frozen control bytes on a real socket, proving the group is
//      actually wired end-to-end.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler for each shape, that a quiescing ingress answers 503
//      {"ok":false,"reason":"shutting-down"} — the INVERTED mutating policy: NEVER a
//      legacy replay, because the refused write lives inside the never-run Effect —
//      that an interrupts-only Exit takes the same 503 refusal, and that a workflow
//      defect reproduces the legacy POST outer-catch 500 {"err":"internal"}.
//
// Every wire byte here is deterministic and IDEMPOTENT: the inputs are unknown-id
// (404) and pure-validation (400) requests, so a legacy-then-workflow capture on
// the same core mutates nothing and agrees to the byte.

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
import test, { type TestContext } from '../helpers/harness-test.ts';

// Port growth: HttpEffectRoutes requires every converted group's builders. The
// control tests only exercise the control group, but installEffectRoutes needs the
// whole port wired, so every group's real builders are folded in here unchanged
// (the non-control groups are never reached in this suite — each has its own
// focused suite: health-state, paste, settings-command-mail-cleanup).
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
} as const;

// ============================ A. ISOLATION ============================

test('controlAsyncWorkflow relays a resolved control result verbatim', async () => {
  const resolved = { status: 202, body: { ok: true, id: 'z9' } };
  let runCalls = 0;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    controlAsyncWorkflow({
      run: () => {
        runCalls += 1;
        return Promise.resolve(resolved);
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  assert.deepEqual(mapEffectRouteExit(exit), { kind: 'success', value: resolved });
  assert.equal(runCalls, 1);
  assert.equal(errCalls, 0);
});

test('controlAsyncWorkflow folds a promise rejection into the legacy 500 wire and logs via onError', async () => {
  const boom = new Error('core promise rejected');
  let seen: unknown;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    controlAsyncWorkflow({
      run: () => Promise.reject(boom),
      onError: (err) => {
        errCalls += 1;
        seen = err;
      },
    }),
  );
  // The rejection is caught INSIDE the workflow and relayed as the SUCCESS wire the
  // legacy `.catch` wrote — never an Effect failure — while onError logs it.
  assert.deepEqual(mapEffectRouteExit(exit), {
    kind: 'success',
    value: { status: 500, body: { ok: false, reason: 'internal' } },
  });
  assert.equal(errCalls, 1);
  assert.equal(seen, boom);
});

test('controlAsyncWorkflow turns a synchronous throw into a die — the outer-catch 500, not the .catch wire', async () => {
  const boom = new Error('threw while building the core promise');
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    controlAsyncWorkflow({
      run: () => {
        throw boom;
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  // A sync throw escapes the promise `.catch` (there is no promise yet); Effect.sync
  // turns it into a die → the transport's defect arm → 500 {"err":"internal"}.
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
  assert.equal(errCalls, 0);
});

test('controlAsyncWorkflow builds lazily — constructing the Effect runs no core call', () => {
  let runCalls = 0;
  // Building the workflow must touch no capability: the core WRITE happens only when
  // the Effect runs, which is exactly why a quiescing ingress (which never runs it)
  // performs no mutation. See the quiesce wire test in section C.
  controlAsyncWorkflow({
    run: () => {
      runCalls += 1;
      return Promise.resolve({ status: 200 });
    },
    onError: () => {},
  });
  assert.equal(runCalls, 0);
});

test('controlSyncWorkflow relays a control result verbatim', () => {
  const result = { status: 409, body: { ok: false, reason: 'bad transition' } };
  let runCalls = 0;
  const out = Effect.runSync(
    controlSyncWorkflow({
      run: () => {
        runCalls += 1;
        return result;
      },
    }),
  );
  assert.equal(out, result); // same reference, relayed verbatim
  assert.equal(runCalls, 1);
});

test('controlSyncWorkflow turns a throw into a die (the outer-catch 500)', async () => {
  const boom = new Error('sync route threw');
  const exit = await Effect.runPromiseExit(
    controlSyncWorkflow({
      run: () => {
        throw boom;
      },
    }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
});

test('questionsDismissWorkflow maps ok→200 / !ok→404 and relays the object as the body', () => {
  const hit = { ok: true, id: 7 };
  assert.deepEqual(Effect.runSync(questionsDismissWorkflow({ run: () => hit })), {
    status: 200,
    body: hit,
  });
  const miss = { ok: false };
  assert.deepEqual(Effect.runSync(questionsDismissWorkflow({ run: () => miss })), {
    status: 404,
    body: miss,
  });
});

test('nameControlWorkflow 400s a non-string suffix without touching the core', () => {
  let validated = 0;
  let applied = 0;
  const out = Effect.runSync(
    nameControlWorkflow({
      clearing: false,
      suffix: 42,
      validateSuffix: () => {
        validated += 1;
        return null;
      },
      applyName: () => {
        applied += 1;
        return { ok: true };
      },
    }),
  );
  assert.deepEqual(out, {
    status: 400,
    body: { ok: false, reason: 'suffix must be a string (or pass {clear:true})' },
  });
  assert.equal(validated, 0);
  assert.equal(applied, 0);
});

test('nameControlWorkflow 400s a suffix the validator rejects, without applying', () => {
  let applied = 0;
  const out = Effect.runSync(
    nameControlWorkflow({
      clearing: false,
      suffix: 'no/slashes',
      validateSuffix: (s) => (s === 'no/slashes' ? 'suffix has a bad char' : null),
      applyName: () => {
        applied += 1;
        return { ok: true };
      },
    }),
  );
  assert.deepEqual(out, { status: 400, body: { ok: false, reason: 'suffix has a bad char' } });
  assert.equal(applied, 0);
});

test('nameControlWorkflow applies a valid suffix and maps ok→200 / !ok→409', () => {
  const okOut = Effect.runSync(
    nameControlWorkflow({
      clearing: false,
      suffix: 'docs-review',
      validateSuffix: () => null,
      applyName: (s) => {
        assert.equal(s, 'docs-review');
        return { ok: true };
      },
    }),
  );
  assert.deepEqual(okOut, { status: 200, body: { ok: true } });

  const conflictOut = Effect.runSync(
    nameControlWorkflow({
      clearing: false,
      suffix: 'docs-review',
      validateSuffix: () => null,
      applyName: () => ({ ok: false }),
    }),
  );
  assert.deepEqual(conflictOut, { status: 409, body: { ok: false } });
});

test('nameControlWorkflow clears without validating (applyName receives null)', () => {
  let validated = 0;
  let appliedWith: unknown = 'unset';
  const out = Effect.runSync(
    nameControlWorkflow({
      clearing: true,
      suffix: undefined,
      validateSuffix: () => {
        validated += 1;
        return 'must not run';
      },
      applyName: (s) => {
        appliedWith = s;
        return { ok: true };
      },
    }),
  );
  assert.deepEqual(out, { status: 200, body: { ok: true } });
  assert.equal(validated, 0);
  assert.equal(appliedWith, null);
});

test('mapEffectRouteExit classifies a control quiesce and interrupt as quiesce, a die as defect', () => {
  // The mutating group leans on the mapper for two policies: a quiescing refusal and
  // an interrupts-only Cause (the shutdown fiber cancelling this in-flight mutation)
  // BOTH classify as quiesce → the transport answers the 503 refusal, never a replay.
  const quiescing = new ApplicationQuiescingError({
    operation: 'POST /api/spawn/:id/kill',
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

// One request over a real loopback socket, capturing status + headers + body so the
// pinnable headers (content-type, content-length, x-content-type-options) can be
// asserted exactly. A body (a control POST) rides as application/json with a byte-
// exact content-length. Never rejects on a non-2xx status.
function rawFull(
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
    const finalHeaders: Record<string, string> = { ...headers };
    if (body !== undefined) {
      if (!('content-type' in finalHeaders)) finalHeaders['content-type'] = 'application/json';
      finalHeaders['content-length'] = String(Buffer.byteLength(body));
    }
    const req = http.request(
      { host: '127.0.0.1', port, path: reqPath, method, headers: finalHeaders },
      (res) => {
        let received = '';
        res.on('data', (d: Buffer) => {
          received += d.toString();
        });
        res.on('end', () => {
          resolve({ status: res.statusCode, headers: res.headers, body: received });
        });
      },
    );
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    if (body !== undefined) req.write(body);
    req.end();
  });
}

// Assert the frozen transport envelope of a control JSON response: status, the
// control dialect (ok:false, which distinguishes a route that ran from the router's
// {"err":"nope"} fall-through), and the pinned header trio incl. content-length.
function assertControlEnvelope(
  res: { status: number; headers: Headers; text: string },
  expectedStatus: number,
  label: string,
): void {
  assert.equal(res.status, expectedStatus, `${label}: status`);
  assert.equal(res.headers.get('content-type'), 'application/json', `${label}: content-type`);
  assert.equal(res.headers.get('x-content-type-options'), 'nosniff', `${label}: nosniff`);
  const cl = res.headers.get('content-length');
  if (cl !== null)
    assert.equal(cl, String(Buffer.byteLength(res.text)), `${label}: content-length`);
  const parsed = JSON.parse(res.text) as Record<string, unknown>;
  assert.equal(
    parsed['ok'],
    false,
    `${label}: control dialect (ok:false), not the 404 fall-through`,
  );
}

test('the wired daemon answers each control shape with the frozen control bytes', async () => {
  const daemon = await startDaemon();
  const post = async (path: string, body: unknown) => {
    const r = await fetch(`${daemon.baseUrl}${path}`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${daemon.token}`,
      },
      body: JSON.stringify(body),
    });
    return { status: r.status, headers: r.headers, text: await r.text() };
  };
  try {
    // controlAsync — an unknown spawn id kills nothing: 404, no mutation.
    assertControlEnvelope(await post('/api/spawn/nope/kill', {}), 404, 'kill unknown id');
    // controlSync — an unknown plan id: 404, no mutation.
    assertControlEnvelope(
      await post('/api/plans/999999/mark', { status: 'archived' }),
      404,
      'plans mark unknown id',
    );
    // questionsDismiss — an unknown question id maps ok:false → 404.
    assertControlEnvelope(
      await post('/api/questions/999999/dismiss', {}),
      404,
      'questions dismiss unknown id',
    );
    // nameControl — a pure-validation 400 (no suffix, not clearing) never calls core.
    assertControlEnvelope(await post('/api/sessions/nope/name', {}), 400, 'name missing suffix');
  } finally {
    await daemon.stop();
  }
  assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
});

// ============================ C. IN-PROCESS EQUIVALENCE ============================

type BoardHandle = ReturnType<typeof createHttp> & { port: number };

// The in-process harness (mirrors the pilot): an idle :memory: core behind
// createHttp, bound on a real loopback port (the Host wall pins Host's port to the
// configured port, so bind a throwaway probe first, then hand createHttp the real
// port). effectRoutes starts null — the legacy path — and the test installs the
// bridge when it wants. token:null + plain loopback authorizes every control POST
// (none is in tokenGatedRoute, requireToken defaults off), so the requests reach the
// route handlers rather than a 401/403 wall.
function startBoard(t: TestContext): Promise<BoardHandle> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  const probe = http.createServer();
  return new Promise<BoardHandle>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const handle = createHttp(core, { port, token: null as unknown as string, lan: null });
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

// Control 404/400 bodies carry no clock field, so the two captures are strictly
// byte-identical — the workflow path and the legacy path must agree to the byte.
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

// The representative routes — one per workflow shape — with idempotent inputs whose
// bytes are identical across a legacy-then-workflow capture on the same idle core.
const PARITY_ROUTES = [
  { label: 'controlAsync (kill unknown id)', path: '/api/spawn/nope/kill', body: '{}' },
  {
    label: 'controlSync (plans mark unknown id)',
    path: '/api/plans/999999/mark',
    body: '{"status":"archived"}',
  },
  {
    label: 'questionsDismiss (unknown id)',
    path: '/api/questions/999999/dismiss',
    body: '{}',
  },
  { label: 'nameControl (missing suffix → 400)', path: '/api/sessions/nope/name', body: '{}' },
  {
    label: 'nameControl (valid suffix, unknown id)',
    path: '/api/sessions/nope/name',
    body: '{"suffix":"docs"}',
  },
] as const;

test('workflow dispatch is byte-identical to the legacy handler for every control shape', async (t) => {
  const board = await startBoard(t);

  // effectRoutes null ⇒ the legacy synchronous handlers answer. Capture each route
  // paired with its result so no indexed lookup is needed downstream.
  const captures: { route: (typeof PARITY_ROUTES)[number]; legacy: RawResponse }[] = [];
  for (const route of PARITY_ROUTES) {
    const legacy = await rawFull(board.port, {
      method: 'POST',
      path: route.path,
      body: route.body,
    });
    // Every route ran (control dialect), not the 404 {"err":"nope"} fall-through.
    const parsed = JSON.parse(legacy.body) as Record<string, unknown>;
    assert.equal(parsed['ok'], false, `${route.label}: legacy control dialect`);
    captures.push({ route, legacy });
  }

  // Wire a FAITHFUL success bridge (runs the real workflow Effect through
  // Effect.runPromiseExit — the same Exit the ingress runtime produces).
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });

  for (const { route, legacy } of captures) {
    const workflow = await rawFull(board.port, {
      method: 'POST',
      path: route.path,
      body: route.body,
    });
    assertByteIdentical(workflow, legacy, route.label);
  }
});

test('a quiescing ingress answers 503 shutting-down — NEVER a legacy replay of the write', async (t) => {
  const board = await startBoard(t);

  // First, the legacy answer for the same route — the 404 a replay WOULD produce.
  const legacyKill = await rawFull(board.port, {
    method: 'POST',
    path: '/api/spawn/nope/kill',
    body: '{}',
  });
  assert.equal(legacyKill.status, 404);

  // The ingress runtime, while quiescing, resolves runRequest to a failed Exit
  // carrying ApplicationQuiescingError WITHOUT running the workflow — so the core
  // write inside that never-run Effect never happens (see the laziness test in
  // section A). The mutating settler must therefore NOT fall back to the legacy
  // handler (that would perform the refused write); it answers the byte-identical
  // 503 the transport gives one tick later once its quiescing flag flips.
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const quiesced = await rawFull(board.port, {
    method: 'POST',
    path: '/api/spawn/nope/kill',
    body: '{}',
  });
  assert.equal(quiesced.status, 503, 'quiesce → 503, not the legacy 404');
  assert.notEqual(quiesced.status, legacyKill.status, 'the 503 must NOT be a legacy replay');
  assert.equal(quiesced.body, '{"ok":false,"reason":"shutting-down"}');
  assert.equal(quiesced.headers['content-type'], 'application/json');
  assert.equal(quiesced.headers['x-content-type-options'], 'nosniff');
});

test('an interrupts-only Exit takes the same 503 refusal as an explicit quiesce', async (t) => {
  const board = await startBoard(t);

  // A mid-flight mutation cancelled by the shutdown fiber yields an interrupts-only
  // Exit; the mapper classifies it as quiesce, so the settler answers 503 — the
  // write was never completed and we say so.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });

  const interrupted = await rawFull(board.port, {
    method: 'POST',
    path: '/api/plans/999999/mark',
    body: '{"status":"archived"}',
  });
  assert.equal(interrupted.status, 503);
  assert.equal(interrupted.body, '{"ok":false,"reason":"shutting-down"}');
  assert.equal(interrupted.headers['content-type'], 'application/json');
  assert.equal(interrupted.headers['x-content-type-options'], 'nosniff');
});

test('a workflow defect reproduces the legacy POST outer-catch 500 {"err":"internal"}', async (t) => {
  const board = await startBoard(t);

  // A die (an unexpected fault) must surface as the byte-identical 500 the legacy
  // POST outer catch emits for a non-hook route — 500 {"err":"internal"}, distinct
  // from the GET snapshot 500 {}, and never the fail-open 200.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(new Error('boom'))),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await rawFull(board.port, {
    method: 'POST',
    path: '/api/spawn/nope/kill',
    body: '{}',
  });
  assert.equal(defected.status, 500);
  assert.equal(defected.body, '{"err":"internal"}');
  assert.equal(defected.headers['content-type'], 'application/json');
  assert.equal(defected.headers['x-content-type-options'], 'nosniff');
});
