// http-workflow-hooks.test.ts — the focused fixture for the P6.4 HOOK route group
// (POST /hook/:name), the final and most sensitive Effect-migration slice. It is
// the companion to tests/p6-hook-failopen-contract.test.ts: that file is a FROZEN
// characterization test that pins the wire behaviour a REAL daemon produces for
// every §5 fail-open row reachable from the network. Several §5 rows it could only
// mark `test.skip(...)` because inducing them requires a source-level fault (a
// thrown hook handler / router error, a mid-shutdown quiesce race) that a
// black-box wire test cannot trigger without editing source. Those are EXACTLY the
// boundary the conversion introduces: the `catchAllCause → 200` fail-open at the
// hook settler. This file makes them mechanically inducible — by injecting the
// non-success Exit at the P6.3 bridge seam (installEffectRoutes' runRequest) on an
// in-process board — and proves each still answers the byte-exact 200 {}:
//
//   A. ISOLATION — mapHookExit (hook-policy.ts) and hookDispatchWorkflow
//      (app/http-workflows/hooks.ts) in pure isolation: every Exit shape → bytes,
//      the three dispatch branches (unknown / invalid / known), and that a handler
//      that THROWS becomes a die that mapHookExit still folds to {}.
//   B. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling a FAITHFUL
//      bridge (Effect.runPromiseExit) proves the workflow path is byte-identical to
//      the legacy dispatch for the unknown / invalid / known branches.
//   C. FAIL-OPEN UNDER AN INJECTED NON-SUCCESS EXIT — the SKIP'd §5 rows made
//      inducible. A defect (die), a submission-throw REJECTION, an interrupts-only
//      interruption, and an ApplicationQuiescingError refusal each resolve the hook
//      to 200 {} — NEVER the 500 the other settlers rethrow a die to, and NEVER the
//      503 the mutating settlers answer a quiesce/interrupt with. This is the whole
//      point of the hook mapper being mapHookExit, not mapEffectRouteExit.
//   D. B2 REPLY FLOOR — a bridge Promise that NEVER settles (a wedged runtime)
//      still answers 200 {}, synthesized by the unref'd idempotent floor timer
//      within the bounded window; nothing else in the daemon would emit those bytes.
//
// CRITICAL FIXTURE NOTE: /hook/* is authenticated UNCONDITIONALLY (authorized() in
// http.ts gates hooks with no loopback exemption, and tokenMatches(null, x) is
// always false). So — unlike the control suite's token:null board — this board is
// built with a REAL token and every hook POST carries `Authorization: Bearer
// <token>`; otherwise every request would hit silentHookRefusal and answer 200 {}
// BEFORE reaching the settler, passing these tests for the wrong reason.

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import * as Cause from 'effect/Cause';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { mapHookExit } from '../../src/daemon/hook-policy.ts';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import {
  controlAsyncWorkflow,
  controlSyncWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
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

import test, { type TestContext } from '../helpers/harness-test.ts';

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// focused fixture exercises only POST /hook/:name; the other groups' builders are
// inert here but keep the install object well-typed.
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
  hookDispatch: hookDispatchWorkflow,
} as const;

// ============================ A. ISOLATION ============================

test('mapHookExit: success carries the assembled body verbatim', () => {
  assert.deepEqual(mapHookExit(Exit.succeed({})), { body: {} });
  assert.deepEqual(mapHookExit(Exit.succeed({ decision: 'block' })), {
    body: { decision: 'block' },
  });
});

test('mapHookExit: EVERY failure shape collapses to { body: {} } — the fail-open boundary', () => {
  // Quiesce refusal — NOT a 503 (that is the mutating groups' policy).
  assert.deepEqual(
    mapHookExit(
      Exit.fail(
        new ApplicationQuiescingError({ operation: 'POST /hook/Stop', message: 'quiescing' }),
      ),
    ),
    { body: {} },
  );
  // Interrupts-only interruption — NOT a 503 either.
  assert.deepEqual(mapHookExit(Exit.failCause(Cause.interrupt(1))), { body: {} });
  // Die (defect) — NOT rethrown to a 500 (that is mapEffectRouteExit's policy).
  assert.deepEqual(mapHookExit(Exit.die(new Error('boom'))), { body: {} });
  // Any other typed fail — still {}.
  assert.deepEqual(mapHookExit(Exit.fail(new Error('unexpected'))), { body: {} });
});

test('hookDispatchWorkflow: building the workflow runs no capability thunk (lazy)', () => {
  let touched = false;
  hookDispatchWorkflow({
    handler: () => {
      touched = true;
      return {};
    },
    valid: () => {
      touched = true;
      return true;
    },
    ingestUnknown: () => {
      touched = true;
    },
  });
  assert.equal(touched, false, 'no thunk runs until the Effect runs');
});

test('hookDispatchWorkflow: unknown event name ingests telemetry once and answers {}', async () => {
  let ingested = 0;
  let handlerCalls = 0;
  const exit = await Effect.runPromiseExit(
    hookDispatchWorkflow({
      handler: null,
      valid: () => {
        handlerCalls += 1; // must NOT be consulted for an unknown name
        return true;
      },
      ingestUnknown: () => {
        ingested += 1;
      },
    }),
  );
  assert.equal(ingested, 1, 'unknown → ingestUnknown exactly once');
  assert.equal(handlerCalls, 0, 'validity is not consulted for an unknown name');
  assert.ok(Exit.isSuccess(exit));
  assert.deepEqual(mapHookExit(exit), { body: {} });
});

test('hookDispatchWorkflow: an invalid payload answers {} WITHOUT dispatch or ingest', async () => {
  let handlerCalls = 0;
  let ingested = 0;
  const exit = await Effect.runPromiseExit(
    hookDispatchWorkflow({
      handler: () => {
        handlerCalls += 1;
        return { decision: 'block' };
      },
      valid: () => false,
      ingestUnknown: () => {
        ingested += 1;
      },
    }),
  );
  assert.equal(handlerCalls, 0, 'invalid → the handler never runs');
  assert.equal(ingested, 0, 'invalid → no unknown-ingest either');
  assert.ok(Exit.isSuccess(exit));
  assert.deepEqual(mapHookExit(exit), { body: {} });
});

test('hookDispatchWorkflow: a known valid handler relays its output, and undefined → {}', async () => {
  const withOutput = await Effect.runPromiseExit(
    hookDispatchWorkflow({
      handler: () => ({ decision: 'block', reason: 'x' }),
      valid: () => true,
      ingestUnknown: () => undefined,
    }),
  );
  assert.deepEqual(mapHookExit(withOutput), { body: { decision: 'block', reason: 'x' } });

  const withUndefined = await Effect.runPromiseExit(
    hookDispatchWorkflow({
      handler: () => undefined,
      valid: () => true,
      ingestUnknown: () => undefined,
    }),
  );
  assert.ok(Exit.isSuccess(withUndefined));
  assert.deepEqual(mapHookExit(withUndefined), { body: {} }, 'handler() ?? {} → {}');
});

test('hookDispatchWorkflow: a handler that THROWS becomes a die that mapHookExit folds to {}', async () => {
  // This is the §5 "POST inner catch on /hook/*" row the contract file could only
  // SKIP (source-level fault injection). Here the throw is a die on the Exit, and
  // the settler's mapHookExit collapses it to the byte-identical fail-open {}.
  const exit = await Effect.runPromiseExit(
    hookDispatchWorkflow({
      handler: () => {
        throw new Error('handler boom');
      },
      valid: () => true,
      ingestUnknown: () => undefined,
    }),
  );
  assert.ok(Exit.isFailure(exit), 'a thrown handler surfaces as a failed (die) Exit');
  assert.deepEqual(mapHookExit(exit), { body: {} });
});

// ======================= B. IN-PROCESS WIRE HARNESS =======================

type BoardHandle = ReturnType<typeof createHttp> & { port: number };

// A fixed, non-null token. Because /hook/* is authed unconditionally, the board
// MUST have a token and every request MUST carry the matching bearer — otherwise
// the request stops at silentHookRefusal before the settler (see the header note).
const TOKEN = 'hook-test-token';

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

// One hook POST over a real loopback socket, always carrying the bearer + a valid
// JSON body, capturing status + headers + body so the pinned header trio
// (content-type, content-length, x-content-type-options) can be asserted exactly.
// Never rejects on a non-2xx status.
function rawHook(
  port: number,
  name: string,
  body: string,
  extraHeaders: Record<string, string> = {},
): Promise<RawResponse> {
  return new Promise<RawResponse>((resolve, reject) => {
    const headers: Record<string, string> = {
      'content-type': 'application/json',
      authorization: `Bearer ${TOKEN}`,
      'content-length': String(Buffer.byteLength(body)),
      ...extraHeaders,
    };
    const req = http.request(
      { host: '127.0.0.1', port, path: `/hook/${name}`, method: 'POST', headers },
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
    req.setTimeout(8000, () => req.destroy(new Error('raw hook request timed out')));
    req.on('error', reject);
    req.write(body);
    req.end();
  });
}

// The canonical hook fail-open assertion — the byte-exact 200 {} + the json()
// header trio, mirroring assertFailOpen in the frozen contract file (content-length
// of `{}` is the string '2').
function assertHookFailOpen(r: RawResponse, label: string): void {
  assert.equal(r.status, 200, `${label}: status must be 200 (got ${r.status}: ${r.body})`);
  assert.equal(
    r.body,
    '{}',
    `${label}: body must be byte-exact {} (got ${JSON.stringify(r.body)})`,
  );
  assert.deepEqual(JSON.parse(r.body), {}, `${label}: parsed body must be {}`);
  assert.equal(r.headers['content-type'], 'application/json', `${label}: content-type trio`);
  assert.equal(r.headers['content-length'], '2', `${label}: content-length of {} is 2`);
  assert.equal(r.headers['x-content-type-options'], 'nosniff', `${label}: nosniff trio`);
}

// The in-process board: an idle :memory: core behind createHttp, bound on a real
// loopback port (bind a throwaway probe first, then hand createHttp the real port,
// so the Host wall's port pin is satisfied). effectRoutes starts null — the legacy
// path — and the test installs the bridge it wants. `floorMs`, when given, is
// exported into the environment ONLY across the createHttp call so the B2 reply
// floor (read at construction) is short enough to observe in section D.
function startBoard(t: TestContext, opts: { floorMs?: number } = {}): Promise<BoardHandle> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home' });
  const probe = http.createServer();
  return new Promise<BoardHandle>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        const key = 'FLEETDECK_HOOK_REPLY_FLOOR_MS';
        const prev = process.env[key];
        if (opts.floorMs !== undefined) process.env[key] = String(opts.floorMs);
        let handle: ReturnType<typeof createHttp>;
        try {
          handle = createHttp(core, { port, token: TOKEN, lan: null });
        } finally {
          if (opts.floorMs !== undefined) {
            if (prev === undefined) delete process.env[key];
            else process.env[key] = prev;
          }
        }
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

// Hook bodies are always literally `{}` (or a handler output) with no clock field,
// so a legacy-then-workflow capture is strictly byte-identical.
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

// One row per dispatch branch. The first three answer exactly {} on BOTH paths (an
// unknown name ingests telemetry then answers {}; an invalid payload answers {}
// without dispatch; FileChanged's handler is the pure `() => ({})`). The last is a
// REAL, non-{} success (NIT 2): SessionStart's handler returns {ok,callsign,brief},
// so byte-identity there proves the workflow path relays a substantial handler
// payload verbatim, not only the empty rows. Every row's body is clock-free and the
// core is idle, so a legacy-then-workflow capture agrees to the byte — the second
// SessionStart reads the same stored card the first created, hence the same callsign
// and brief.
const PARITY_ROWS = [
  {
    label: 'unknown-name (Bogus → ingest + {})',
    name: 'Bogus',
    body: '{"session_id":"s"}',
    empty: true,
  },
  {
    label: 'invalid-payload (PreToolUse, missing session_id → {})',
    name: 'PreToolUse',
    body: '{"tool_name":"Bash"}',
    empty: true,
  },
  {
    label: 'known-handler (FileChanged → {})',
    name: 'FileChanged',
    body: '{"session_id":"s"}',
    empty: true,
  },
  {
    label: 'known-handler (SessionStart → {ok,callsign,brief})',
    name: 'SessionStart',
    body: '{"session_id":"s"}',
    empty: false,
  },
] as const;

test('workflow dispatch is byte-identical to the legacy handler for every hook branch', async (t) => {
  const board = await startBoard(t);

  // effectRoutes null ⇒ the legacy synchronous dispatch answers. Capture each row
  // paired with its legacy bytes.
  const captures: { row: (typeof PARITY_ROWS)[number]; legacy: RawResponse }[] = [];
  for (const row of PARITY_ROWS) {
    const legacy = await rawHook(board.port, row.name, row.body);
    if (row.empty) {
      assertHookFailOpen(legacy, `${row.label}: legacy`);
    } else {
      // A non-{} success row: still 200, but carries a real handler payload. Pin
      // that it is the {ok,callsign,brief} success (not an accidental fail-open {}),
      // then let assertByteIdentical below prove the workflow reproduces it exactly.
      assert.equal(legacy.status, 200, `${row.label}: legacy status must be 200`);
      assert.notEqual(legacy.body, '{}', `${row.label}: legacy must carry a real payload`);
      assert.equal(
        (JSON.parse(legacy.body) as { ok?: unknown }).ok,
        true,
        `${row.label}: legacy body is the {ok,callsign,brief} success`,
      );
    }
    captures.push({ row, legacy });
  }

  // Wire a FAITHFUL success bridge (the real workflow Effect through
  // Effect.runPromiseExit — the same Exit the ingress runtime produces).
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });

  for (const { row, legacy } of captures) {
    const workflow = await rawHook(board.port, row.name, row.body);
    assertByteIdentical(workflow, legacy, row.label);
  }
});

// ============ C. FAIL-OPEN UNDER AN INJECTED NON-SUCCESS EXIT ============
// The bridge seam (installEffectRoutes' runRequest) is where the ingress runtime
// hands the settled Exit back. Injecting each non-success shape here reproduces the
// §5 rows the frozen contract could only SKIP — and pins that the hook settler
// diverges from BOTH the snapshot (legacy replay) and mutating (503 / 500) policies.

// The 503 shutting-down body the MUTATING settlers answer a quiesce/interrupt with —
// asserted-against here so the divergence (hooks fail open instead) is explicit.
const SHUTTING_DOWN = '{"ok":false,"reason":"shutting-down"}';

test('a workflow DEFECT (die) fails open to 200 {} — never the 500 the other settlers rethrow to', async (t) => {
  const board = await startBoard(t);
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(new Error('boom'))),
    ...ALL_ROUTE_BUILDERS,
  });
  const r = await rawHook(board.port, 'Notification', '{"session_id":"s"}');
  assertHookFailOpen(r, 'defect');
  assert.notEqual(r.body, '{"err":"internal"}', 'a hook die is NOT the POST outer-catch 500');
  assert.notEqual(r.status, 500, 'a hook die never surfaces a 500');
});

test('a submission-throw REJECTION fails open to 200 {} (the .catch arm — B1 submission path)', async (t) => {
  const board = await startBoard(t);
  // The ONLY way the P6.3 bridge Promise REJECTS is a synchronous submission throw;
  // hooks still fail open, from the settler's .catch arm.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.reject(new Error('submission boom')),
    ...ALL_ROUTE_BUILDERS,
  });
  const r = await rawHook(board.port, 'Notification', '{"session_id":"s"}');
  assertHookFailOpen(r, 'submission-rejection');
});

test('an interrupts-only INTERRUPTION fails open to 200 {} — NOT the 503 a mutating route answers', async (t) => {
  const board = await startBoard(t);
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });
  const r = await rawHook(board.port, 'Notification', '{"session_id":"s"}');
  assertHookFailOpen(r, 'interruption');
  assert.notEqual(r.body, SHUTTING_DOWN, 'a hook interrupt fails OPEN, not the 503 refusal');
  assert.notEqual(r.status, 503, 'a hook interrupt never answers 503');
});

test('a quiescing ingress (ApplicationQuiescingError) fails open to 200 {} — NOT the 503 refusal', async (t) => {
  const board = await startBoard(t);
  // The unique hook policy: even a quiesce REFUSAL fails open. A hook fired during
  // transport quiesce must never see a 503 (contrast the mutating settlers, which
  // answer exactly SHUTTING_DOWN here).
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });
  const r = await rawHook(board.port, 'Stop', '{"session_id":"s"}');
  assertHookFailOpen(r, 'quiesce');
  assert.notEqual(r.body, SHUTTING_DOWN, 'a hook quiesce fails OPEN, not the 503 refusal');
  assert.notEqual(r.status, 503, 'a hook quiesce never answers 503');
});

// ======================= D. B2 REPLY FLOOR =======================

test('a bridge Promise that NEVER settles still answers 200 {} via the B2 reply floor', async (t) => {
  // A wedged Effect runtime: runRequest returns a Promise that never resolves. No
  // other daemon machinery would emit the contract bytes (active requests are
  // idleTimeout 0, the keep-alive FINs write no body). The unref'd idempotent floor
  // synthesizes 200 {} after FLEETDECK_HOOK_REPLY_FLOOR_MS (set short here).
  const floorMs = 200;
  const board = await startBoard(t, { floorMs });
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => new Promise<never>(() => undefined),
    ...ALL_ROUTE_BUILDERS,
  });
  const started = Date.now();
  const r = await rawHook(board.port, 'Notification', '{"session_id":"s"}');
  const elapsed = Date.now() - started;
  assertHookFailOpen(r, 'b2-floor');
  // The reply came from the FLOOR, not the (never-settling) bridge: it waited out
  // the floor window rather than resolving immediately.
  assert.ok(
    elapsed >= floorMs - 80,
    `the floor reply must wait ~${floorMs}ms, not resolve immediately (elapsed ${elapsed}ms)`,
  );
});

// ============ E. AN UNSERIALIZABLE SUCCESS FAILS OPEN (Finding 1) ============
// mapHookExit relays a Success body VERBATIM, so a workflow that succeeds with a
// value json() cannot serialize — JSON.stringify turns it into `undefined`, or the
// value is circular — makes the settler's json() throw BEFORE a byte is written.
// The emitter must still answer the canonical 200 {} PROMPTLY, from its catch, not
// strand the request until (or past) the B2 floor. A floor comfortably longer than
// a loopback round-trip proves the reply beat it: with the pre-fix emitter (settled
// flipped + floor cleared BEFORE json threw) the floor was already cancelled and the
// request would hang to the socket timeout instead of answering.

test('an unserializable Success (undefined / circular) fails open to 200 {} promptly — not via the floor', async (t) => {
  const floorMs = 1000;

  // A body whose JSON.stringify THROWS (circular), plus one it turns into undefined.
  const circular: { self?: unknown } = {};
  circular.self = circular;
  const rows: { label: string; value: unknown }[] = [
    { label: 'undefined-success (JSON.stringify → undefined)', value: undefined },
    { label: 'circular-success (JSON.stringify throws)', value: circular },
  ];

  for (const { label, value } of rows) {
    const board = await startBoard(t, { floorMs });
    // The bridge resolves a SUCCESS whose value json() cannot serialize; mapHookExit
    // hands it to the emitter verbatim, so the emitter's own catch is the only thing
    // that can still produce the contract bytes.
    board.installEffectRoutes({
      runRequest: (_operation, _effect) => Promise.resolve(Exit.succeed(value)),
      ...ALL_ROUTE_BUILDERS,
    });
    const started = Date.now();
    const r = await rawHook(board.port, 'Notification', '{"session_id":"s"}');
    const elapsed = Date.now() - started;
    assertHookFailOpen(r, label);
    // The 200 {} came from the emitter's catch, not the floor: it resolved well
    // under the floor window (the pre-fix emitter would have hung here instead).
    assert.ok(
      elapsed < floorMs / 2,
      `${label}: the 200 {} must come from the emitter's catch, not the ${floorMs}ms floor (elapsed ${elapsed}ms)`,
    );
  }
});
