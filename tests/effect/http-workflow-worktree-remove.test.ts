// http-workflow-worktree-remove.test.ts — the focused fixture for the P9.2 Slice 3
// REMOVE route POST /api/worktrees/remove: the allow-listed, destructive worktree
// removal. It pins the CONVENTION in src/daemon/app/http-workflows/worktrees.ts's
// worktreeRemoveWorkflow the same five ways the repos / control / worktrees-read
// suites pin theirs, PLUS the remove-specific GAP-3a/GAP-3b split (slice-0
// characterization) and a CORE dispatcher-liveness pin for the degenerate/hybrid
// Effect core in worktrees.ts:
//
//   A. ISOLATION — worktreeRemoveWorkflow and the Exit → outcome mapper in pure
//      isolation with capability fakes: a resolved wire (200 removed, 409 refusal)
//      is relayed VERBATIM as the SUCCESS value — expected failures are DATA, so
//      E=never and mapEffectRouteExit grows no case; a RESOLVED purge-path 500
//      {ok:false,reason:`could not purge worktree rows: …`} ALSO rides the success
//      channel verbatim with NO onError (GAP-3b); a promise REJECTION is folded
//      INSIDE the workflow to the generic 500 wire {ok:false,reason:'internal'}
//      (onError logging the frozen removal line), surfacing as SUCCESS — never an
//      Effect failure (GAP-3a); lazy construction; and the (structurally
//      unreachable — removeWorktree is a Promise-returning dispatcher) synchronous
//      throw that dies to the settler's 500 REMOVE_DEFECT arm.
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires worktreeRemove to the live
//      ingress bridge AND injects runControlDetached into the worktrees ctx) answers
//      POST /api/worktrees/remove on a real socket with a deterministic 400 DATA
//      dialect (a string path absent from spawns → 'not a fleet worktree', decided
//      by the allow-list in the Effect-core run tail before any git op), proving the
//      group is wired end-to-end through BOTH the Effect core AND the Effect transport.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler for a 200 removed wire and a 409 refusal wire, that a RESOLVED
//      purge-500 passes through onFulfilled verbatim with NO removal log on BOTH
//      paths (GAP-3b), that a STUBBED core rejection folds to the generic 500
//      dialect + the removal log on BOTH transport paths (byte- AND log-identical
//      — GAP-3a; the REAL dispatcher-tail rejection identity is the D-section pin),
//      that a quiescing ingress REFUSES
//      with 503 {ok:false,reason:'shutting-down'} (remove is a MUTATING-family POST —
//      NOT a read replay), and that a workflow defect renders the SAME 500 removal
//      dialect + the removal log.
//   D. CORE DISPATCHER LIVENESS — the worktrees.ts hybrid dispatcher routes through
//      the injected runControlDetached when present (runner invoked exactly once)
//      and falls to the verbatim legacy body when absent, both producing the
//      identical wire. Driven with a network- AND db-free 400 target ({} → the
//      removeWorktreeStep sync gate 'not a fleet worktree' fires before any lock,
//      git, or db read), so the wire is deterministic on BOTH dispatcher legs.
//      A second pin induces a throw PAST that sync gate (string path, then
//      q.worktreeSpawns.all throws) so runner-present and runner-absent both
//      reject with the RAW error identically — the §6 defect-identity C cannot
//      see because it stubs core.removeWorktree.
//   E. LIVE JOIN — admission succeeds, startOnce captures the native write, the
//      request fiber is interrupted → the settler JOINS (never 503). A joined
//      native REJECTION is rendered by onRejected as the generic removal 500 dialect
//      + the removal log. Modeled on the preflight / control JOIN pins.
//
// DIALECT NOTE. remove's fold body { ok: false, reason: 'internal' } is BYTE-EQUAL to
// controlAsync's 500 body — the two routes are differentiated NOT by wire bytes but
// by the LOG PREFIX ('fleetd worktree removal error:' vs controlAsync's line) and by
// the settler that owns them. So the guards below assert the fold is NOT
// PREFLIGHT_DEFECT's 'Git access check failed internally' and NOT CONTROL_DEFECT's
// {err:'internal'}, and pin the removal log line by identity; they do NOT assert
// byte-inequality with controlAsync (which would be false). The remove-specific
// contract that DOES move wire bytes is GAP-3b: a RESOLVED purge-500 is a SUCCESS
// wire (rides onFulfilled, no log), byte-distinct from the GAP-3a rejection fold.
//
// Every wire byte here is deterministic and network-free: sections A/C/E use
// capability / core stubs, section B's 400 is decided by the empty-db allow-list
// before any git op, and section D's 400 is the sync gate before any ctx use.

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createWorktrees } from '../../src/daemon/worktrees.ts';
import { type RunControlDetached } from '../../src/daemon/retention.ts';
import {
  makeIngressSupervisor,
  runControlDetached,
} from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import type { IngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { mapEffectRouteExit } from '../../src/daemon/http-policy.ts';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import {
  armUnsupervisedWorkflow,
  controlAsyncWorkflow,
  controlSyncWorkflow,
  mailAckWorkflow,
  mailDrainWorkflow,
  nameControlWorkflow,
  questionsDismissWorkflow,
  spawnRouteWorkflow,
} from '../../src/daemon/app/http-workflows/control.ts';
import {
  healthWorkflow,
  settingsSnapshotWorkflow,
  stateWorkflow,
} from '../../src/daemon/app/http-workflows/health-state.ts';
import { hookDispatchWorkflow } from '../../src/daemon/app/http-workflows/hooks.ts';
import { pasteImageWorkflow } from '../../src/daemon/app/http-workflows/paste.ts';
import {
  cleanupWorkflow,
  commandWorkflow,
  mailWorkflow,
  settingsWorkflow,
} from '../../src/daemon/app/http-workflows/settings-command-mail-cleanup.ts';
import {
  worktreesSnapshotWorkflow,
  worktreeRemoveWorkflow,
  type WorktreeRemoveWire,
} from '../../src/daemon/app/http-workflows/worktrees.ts';
import { repoPreflightWorkflow } from '../../src/daemon/app/http-workflows/repos.ts';
import { heldSettleWorkflow } from '../../src/daemon/app/http-workflows/held.ts';

import { startDaemon } from '../helpers/daemon.ts';
import { postJson } from '../helpers/http.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// suite only exercises the worktree-remove group, but installEffectRoutes needs the
// whole port wired, so every group's real builders are folded in here unchanged (the
// non-remove groups are never reached in this suite — each has its own focused suite).
const ALL_ROUTE_BUILDERS = {
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
  watchHold: heldSettleWorkflow,
  hookHold: heldSettleWorkflow,
  // Production held runner (untracked, runControlDetached = Effect.runPromiseWith
  // (Context.empty())). A die REJECTS the Promise; it does NOT squash to a value.
  // Hold fail-open safety is the settler's .catch (settleEffectWatchHold /
  // settleEffectHookHold), not this runner. Not a runPromiseExit stub; see
  // http-workflow-control.test.ts for the full rationale (finding 3).
  runHeld: runControlDetached,
} as const;

// The frozen generic remove 500 wire — the fold body AND the settler defect body
// (slice-0 GAP-3a). BYTE-EQUAL to controlAsync's fold; the removal LOG is what makes
// it distinct on the wire's audit side.
const REMOVE_500 = '{"ok":false,"reason":"internal"}';
// The frozen removal log prefix — shared by the legacy `.catch`, the workflow's
// onError, and the settler's defect arm.
const REMOVE_LOG = 'fleetd worktree removal error:';
// The dialects the remove route must NEVER collapse into.
const PREFLIGHT_500 = '{"ok":false,"reason":"Git access check failed internally"}';
const CONTROL_DEFECT_500 = '{"err":"internal"}'; // CONTROL_DEFECT arm
const SHUTDOWN_503 = '{"ok":false,"reason":"shutting-down"}';

// The remove route emits NO logExec audit line (unlike preflight), so the only
// console.error a request can produce is the removal-error fold. The pins still
// filter the capture to the REMOVE_LOG line — the slice-0 `logged.find` technique —
// so an unrelated line could never be mistaken for a removal error.
function removeLogs(errors: unknown[][]): unknown[][] {
  return errors.filter((a) => a[0] === REMOVE_LOG);
}

// Capture console.error for the duration of one async request. onError (or the
// settler defect arm) logs BEFORE json() writes the response, and the raw request
// resolves only after the response ends, so a capture around the awaited request
// sees every removal line the path emitted.
async function withErrorLog<T>(fn: () => Promise<T>): Promise<{ result: T; errors: unknown[][] }> {
  const errors: unknown[][] = [];
  const original = console.error;
  console.error = (...args: unknown[]): void => {
    errors.push(args);
  };
  try {
    const result = await fn();
    return { result, errors };
  } finally {
    console.error = original;
  }
}

// A RESOLVED purge-path 500 (worktrees.ts BEGIN IMMEDIATE catch) — a { status, body }
// the core RETURNS, not a rejection. Its body template is reproduced here so the
// anchor tracks the source expression byte-for-byte (slice-0 GAP-3b).
const PURGE_DETAIL = 'disk I/O error';
const PURGE_500_WIRE = {
  status: 500,
  body: { ok: false, reason: `could not purge worktree rows: ${PURGE_DETAIL}` },
} as const;
const PURGE_500_BODY = JSON.stringify(PURGE_500_WIRE.body);

// ============================ A. ISOLATION ============================

test('worktreeRemoveWorkflow relays a resolved 200 removed wire verbatim (success is DATA, E=never)', async () => {
  const wire: WorktreeRemoveWire = {
    status: 200,
    body: { ok: true, removed: true, branch_deleted: false, rows_purged: 2, path: '/tree/otter' },
  };
  let runCalls = 0;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreeRemoveWorkflow({
      run: () => {
        runCalls += 1;
        return Promise.resolve(wire);
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'success');
  assert.equal(outcome.kind === 'success' ? outcome.value : null, wire); // same reference
  assert.equal(runCalls, 1);
  assert.equal(errCalls, 0);
});

test('worktreeRemoveWorkflow relays a 409 refusal wire verbatim (expected failure is DATA, not an Effect error)', async () => {
  const wire: WorktreeRemoveWire = {
    status: 409,
    body: { ok: false, verdict: 'has-work', dirty: 3, unpushed: 0 },
  };
  const exit = await Effect.runPromiseExit(
    worktreeRemoveWorkflow({ run: () => Promise.resolve(wire), onError: () => {} }),
  );
  assert.deepEqual(mapEffectRouteExit(exit), { kind: 'success', value: wire });
});

test('worktreeRemoveWorkflow relays a RESOLVED purge-500 verbatim as SUCCESS with NO onError (GAP-3b)', async () => {
  // The purge-path 500 is an EXPECTED outcome the core already encodes as a
  // { status, body } wire — it rides the success channel verbatim, never lifted into
  // a typed error and never confused with the rejection fold, and onError is NEVER
  // called (a resolved wire is not a `.catch`).
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreeRemoveWorkflow({
      run: () => Promise.resolve(PURGE_500_WIRE),
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'success');
  assert.equal(outcome.kind === 'success' ? outcome.value : null, PURGE_500_WIRE); // same reference
  assert.equal(errCalls, 0, 'a resolved purge-500 is not a fold — onError never runs');
});

test('worktreeRemoveWorkflow folds a promise rejection into the generic 500 wire and logs via onError (GAP-3a)', async () => {
  const boom = new Error('remove core exploded');
  let seen: unknown;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreeRemoveWorkflow({
      run: () => Promise.reject(boom),
      onError: (err) => {
        errCalls += 1;
        seen = err;
      },
    }),
  );
  // The rejection is caught INSIDE the workflow and relayed as the SUCCESS wire the
  // legacy `.catch` wrote — never an Effect failure — while onError logs it.
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'success');
  const value =
    outcome.kind === 'success' ? (outcome.value as { status: number; body: unknown }) : null;
  assert.deepEqual(value, { status: 500, body: { ok: false, reason: 'internal' } });
  assert.equal(errCalls, 1);
  assert.equal(seen, boom); // identity preserved through the fold
  // Dialect guards: NOT preflight's distinct 500, NOT CONTROL_DEFECT's {err:internal}.
  assert.notEqual(JSON.stringify(value?.body), PREFLIGHT_500);
  assert.notEqual(JSON.stringify(value?.body), CONTROL_DEFECT_500);
});

test('worktreeRemoveWorkflow builds lazily — constructing the Effect runs no core call', () => {
  let runCalls = 0;
  worktreeRemoveWorkflow({
    run: () => {
      runCalls += 1;
      return Promise.resolve({ status: 200 });
    },
    onError: () => {},
  });
  assert.equal(runCalls, 0);
});

test('worktreeRemoveWorkflow turns a synchronous throw into a die (500 defect arm, structurally unreachable)', async () => {
  const boom = new Error('threw while starting the core promise');
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreeRemoveWorkflow({
      run: () => {
        throw boom;
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  // A sync throw escapes the promise fold (there is no promise yet); Effect.sync
  // turns it into a die → the transport's defect arm (500 removal dialect — see the
  // defect wire test in section C). STRUCTURALLY UNREACHABLE in production:
  // removeWorktree is a Promise-returning dispatcher whose both legs return a
  // Promise, so Effect.sync cannot throw. onError never runs on this arm (no
  // rejection was folded), so the settler's own removal log covers it.
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
  assert.equal(errCalls, 0);
});

// ============================ B. REAL DAEMON WIRE ============================

test('the wired daemon answers POST /api/worktrees/remove through the effect route (deterministic 400 DATA)', async () => {
  const daemon = await startDaemon();
  try {
    // A string path absent from the (empty) spawns allow-list is a 400 DATA outcome
    // decided INSIDE the Effect-core run tail (worktreeRows() → not found) BEFORE any
    // git op, so the wire is deterministic AND env-independent. A fall-through (route
    // not wired) would be a 404/405, not this dialect — so a 400 with this exact
    // reason proves the group is wired end-to-end through the Effect core AND transport.
    const r = await postJson(
      `${daemon.baseUrl}/api/worktrees/remove`,
      { path: '/tmp/not-a-fleet-worktree-xyz' },
      { token: daemon.token },
    );
    assert.equal(r.status, 400, 'remove of an unowned path → 400 DATA');
    assert.deepEqual(r.json, { ok: false, reason: 'not a fleet worktree' });
    // Not a fall-through and not the 500 fold dialect.
    assert.notEqual(r.text, REMOVE_500, 'a DATA 400, not the 500 fold');
  } finally {
    await daemon.stop();
  }
  assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
});

// ============================ C. IN-PROCESS EQUIVALENCE ============================

type BoardHandle = ReturnType<typeof createHttp> & {
  port: number;
  core: ReturnType<typeof createCore>;
};

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

// One POST over a real loopback socket, capturing status + headers + body so the
// pinnable headers (content-type, content-length, x-content-type-options) can be
// asserted exactly. Never rejects on a non-2xx status.
function rawPost(port: number, reqPath: string, payload: unknown): Promise<RawResponse> {
  const data = JSON.stringify(payload ?? {});
  return new Promise<RawResponse>((resolve, reject) => {
    const req = http.request(
      {
        host: '127.0.0.1',
        port,
        path: reqPath,
        method: 'POST',
        headers: { 'content-type': 'application/json', 'content-length': Buffer.byteLength(data) },
      },
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
    req.end(data);
  });
}

// The in-process harness (mirrors the repos / control / worktrees suites): an idle
// :memory: core behind createHttp, bound on a real loopback port. effectRoutes starts
// null — the legacy path — and the test installs the bridge when it wants. token:null
// + plain loopback authorizes the POST, so the request reaches the route handler
// rather than a 401 wall. The core is returned so a test can override
// core.removeWorktree to force a specific wire / rejection.
function startBoard(
  t: TestContext,
  opts?: { runControlDetached?: RunControlDetached },
): Promise<BoardHandle> {
  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 0,
    home: '/daemon-home',
    runControlDetached: opts?.runControlDetached ?? runControlDetached,
  });
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
          resolve({ ...handle, port, core });
        });
      });
    });
  });
}

// The remove wires carry no clock field, so the two captures are strictly
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

// A stubbed wire is relayed byte-identically on the legacy AND the effect path. The
// two DATA dialects (200 removed, 409 refusal) both ride the success channel, so this
// is the dual-path parity for every non-fold outcome.
for (const dialect of [
  {
    name: '200 removed',
    wire: {
      status: 200,
      body: { ok: true, removed: true, branch_deleted: true, rows_purged: 2, path: '/tree/otter' },
    },
  },
  {
    name: '409 refusal',
    wire: { status: 409, body: { ok: false, reason: 'session is still alive' } },
  },
] as const) {
  test(`workflow dispatch is byte-identical to the legacy handler for a ${dialect.name} wire`, async (t) => {
    const board = await startBoard(t);
    board.core.removeWorktree = () => Promise.resolve(dialect.wire);

    // effectRoutes null ⇒ the legacy `.then(json)` handler answers — no log.
    const legacyRun = await withErrorLog(() =>
      rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter', force: true }),
    );
    const legacy = legacyRun.result;
    assert.equal(legacy.status, dialect.wire.status, `legacy ${dialect.name} status`);
    assert.deepEqual(removeLogs(legacyRun.errors), [], 'legacy success arm logs no removal error');

    // Wire the FAITHFUL success bridge (runs the real workflow Effect through
    // Effect.runPromiseExit — the same Exit the ingress runtime produces).
    board.installEffectRoutes({
      runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
      ...ALL_ROUTE_BUILDERS,
    });

    const workflowRun = await withErrorLog(() =>
      rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter', force: true }),
    );
    assertByteIdentical(workflowRun.result, legacy, `remove ${dialect.name}`);
    assert.deepEqual(
      removeLogs(workflowRun.errors),
      [],
      'workflow success arm logs no removal error',
    );
  });
}

test('a RESOLVED purge-500 passes through onFulfilled verbatim on BOTH paths — byte-distinct from the fold, NO removal log (GAP-3b)', async (t) => {
  const board = await startBoard(t);
  board.core.removeWorktree = () => Promise.resolve(PURGE_500_WIRE);

  // Legacy path (effectRoutes null): the `.then(json)` relays the resolved wire — the
  // 500 status is DATA, not a `.catch` fold, so nothing is logged.
  const legacyRun = await withErrorLog(() =>
    rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' }),
  );
  const legacy = legacyRun.result;
  assert.equal(legacy.status, 500, 'legacy purge-500 status');
  assert.equal(legacy.body, PURGE_500_BODY, 'legacy relays the purge-500 body verbatim');
  assert.notEqual(legacy.body, REMOVE_500, 'the purge-500 is byte-distinct from the generic fold');
  assert.deepEqual(removeLogs(legacyRun.errors), [], 'a resolved purge-500 is not a fold — no log');

  // Effect path: the workflow relays the same resolved wire through onFulfilled, still
  // byte-identical, still no removal log (onError never runs on a resolve).
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
  const workflowRun = await withErrorLog(() =>
    rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' }),
  );
  assertByteIdentical(workflowRun.result, legacy, 'remove purge-500 passthrough');
  assert.deepEqual(
    removeLogs(workflowRun.errors),
    [],
    'the effect path also treats the purge-500 as data — no removal log',
  );
});

test('a core rejection folds to the generic 500 removal dialect + the removal log on BOTH paths, byte- and log-identical (GAP-3a)', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('remove probe boom');
  // Force the fold on both paths: the workflow's run capability and the legacy
  // handler both call this same rejecting core.removeWorktree.
  board.core.removeWorktree = () => Promise.reject(boom);

  // Legacy path (effectRoutes null): the `.catch` folds to the 500 dialect + logs.
  const legacyRun = await withErrorLog(() =>
    rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' }),
  );
  const legacy = legacyRun.result;
  assert.equal(legacy.status, 500, 'legacy fold → 500');
  assert.equal(legacy.body, REMOVE_500, 'legacy fold → the generic removal dialect');
  assert.notEqual(legacy.body, PREFLIGHT_500, "never preflight's distinct 500");
  assert.notEqual(legacy.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.deepEqual(
    removeLogs(legacyRun.errors),
    [[REMOVE_LOG, boom]],
    'legacy logs the removal line once',
  );

  // Effect path: the workflow folds the same rejection to the same 500 dialect, and
  // its onError logs the identical line — the settler answers via its success branch.
  // TRANSPORT dual-path GAP-3a pin (legacy `.catch` vs workflow onError) with a
  // STUBBED core.removeWorktree — it does not enter removeWorktreeEffect /
  // removeWorktreeLegacy. The real dispatcher-tail rejection identity (runner
  // present vs absent, causeSquash) is the D-section pin below.
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
  const workflowRun = await withErrorLog(() =>
    rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' }),
  );
  assertByteIdentical(workflowRun.result, legacy, 'remove fold');
  assert.deepEqual(
    removeLogs(workflowRun.errors),
    [[REMOVE_LOG, boom]],
    'workflow logs the identical removal line once',
  );
});

test('a quiescing ingress REFUSES the remove POST with 503 shutting-down (mutating-family, NOT a read replay)', async (t) => {
  const board = await startBoard(t);

  // Remove is a MUTATING-family POST: the workflow effect is never run (the ingress
  // admission refuses), the recorder never starts the native call, so the settler
  // emits the frozen shutdown 503 — the INVERTED policy vs the worktrees READ, which
  // replays to a 200.
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const quiesced = await rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' });
  assert.equal(quiesced.status, 503, 'quiesce → 503 refusal');
  assert.equal(quiesced.body, SHUTDOWN_503, 'quiesce → the frozen shutdown wire');
  assert.equal(quiesced.headers['content-type'], 'application/json');
  assert.equal(quiesced.headers['x-content-type-options'], 'nosniff');
});

test('a workflow defect renders the SAME 500 removal dialect + the removal log — never CONTROL_DEFECT, never 503', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('unexpected remove fault');

  // A die (the structurally-unreachable sync-throw arm from section A, or any
  // unexpected fault) surfaces the SAME 500 removal dialect the fold does — NOT
  // CONTROL_DEFECT's {"err":"internal"}, NOT preflight's distinct 500, NOT the 503
  // refusal. The recorder never started (runRequest ignores the effect), so the
  // settler throws the defect into its `.catch`, logging the frozen removal line so a
  // real regression is not swallowed silently.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(boom)),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await withErrorLog(() =>
    rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter' }),
  );
  assert.equal(defected.result.status, 500, 'defect → 500, never 503');
  assert.equal(defected.result.body, REMOVE_500, 'defect → the generic removal dialect');
  assert.notEqual(defected.result.body, PREFLIGHT_500, "never preflight's distinct 500");
  assert.notEqual(defected.result.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.notEqual(defected.result.body, SHUTDOWN_503, 'a defect is not a quiesce refusal');
  assert.equal(defected.result.headers['content-type'], 'application/json');
  assert.equal(defected.result.headers['x-content-type-options'], 'nosniff');
  assert.deepEqual(
    removeLogs(defected.errors),
    [[REMOVE_LOG, boom]],
    'the settler logs the defect with the frozen removal line',
  );
});

// ============================ D. CORE DISPATCHER LIVENESS ============================

// removeWorktreeStep's sync gate answers before any ctx field is touched, so a
// minimal fake ctx suffices (createWorktrees construction is pure — it only
// destructures ctx and defines closures). The optional runControlDetached is the
// slice-3 seam — present ⇒ the hybrid Effect core runs.
function fakeRemoveCtx(runner?: RunControlDetached) {
  const base = {
    q: {},
    tick: () => {
      /* no-op tick for the fake ctx */
    },
    onMutate: () => {
      /* no-op mutation hook for the fake ctx */
    },
  };
  return (runner ? { ...base, runControlDetached: runner } : base) as unknown as Parameters<
    typeof createWorktrees
  >[0];
}

test('CORE: the removeWorktree dispatcher routes through the injected runner when present, and matches the legacy body when absent', async () => {
  // A network- AND db-free 400: an empty body ({} → path is undefined) is rejected by
  // the removeWorktreeStep sync gate BEFORE any lock, git, or db read — so the wire is
  // deterministic on BOTH dispatcher legs.
  const EXPECT = { status: 400, body: { ok: false, reason: 'not a fleet worktree' } };

  // runner PRESENT ⇒ the dispatcher runs the hybrid Effect core through the injected
  // runControlDetached (Effect.sync over removeWorktreeStep, dischargeStep → succeed).
  let runnerCalls = 0;
  const spy: RunControlDetached = (effect) => {
    runnerCalls += 1;
    return runControlDetached(effect);
  };
  const wired = createWorktrees(fakeRemoveCtx(spy));
  const wiredOut = await wired.removeWorktree({});
  assert.equal(runnerCalls, 1, 'runner invoked exactly once (Effect core is live)');
  assert.deepEqual(wiredOut, EXPECT, 'Effect-core dispatch → the deterministic 400 wire');

  // runner ABSENT ⇒ the dispatcher falls to the verbatim legacy body — the rollback
  // seam — producing the byte-identical wire.
  const legacy = createWorktrees(fakeRemoveCtx());
  const legacyOut = await legacy.removeWorktree({});
  assert.deepEqual(legacyOut, EXPECT, 'runner-absent → the verbatim legacy body');
  assert.deepEqual(legacyOut, wiredOut, 'both dispatcher legs agree to the byte');
});

// Capture a promise's rejection value (identity-preserved), or a sentinel marking
// that it unexpectedly resolved. Mirrors repos-core-parity / worktrees-core-parity.
const NO_REJECTION = Symbol('no-rejection');
async function rejectionOf(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
    return NO_REJECTION;
  } catch (err) {
    return err;
  }
}

test('CORE: a tail throw past the sync gate rejects with the RAW error identically on runner-present and runner-absent', async () => {
  // Unowned-path is DATA 400 (the allow-list miss inside the tail), so it cannot
  // pin defect-identity. Induce a throw at the first tail seam after the sync
  // gate: a STRING path passes removeWorktreeStep, then q.worktreeSpawns.all()
  // throws. Effect arm: Effect.promise-die → runControlDetached causeSquash.
  // Legacy arm (runner absent ≡ EFFECT_CORE_WORKTREES_REMOVE false rollback):
  // the async body's own rejection. Both must surface the SAME object.
  const sentinel = new Error('remove-dispatcher-tail reject sentinel');
  function throwingTailCtx(runner?: RunControlDetached) {
    const base = {
      q: {
        worktreeSpawns: {
          all: (): never => {
            throw sentinel;
          },
        },
      },
      tick: () => {
        /* no-op tick for the throwing ctx */
      },
      onMutate: () => {
        /* no-op mutation hook for the throwing ctx */
      },
    };
    return (runner ? { ...base, runControlDetached: runner } : base) as unknown as Parameters<
      typeof createWorktrees
    >[0];
  }

  let n = 0;
  const wrapping: RunControlDetached = (effect) => {
    n += 1;
    return runControlDetached(effect);
  };

  // String path → past the sync gate, into runRemoveWorktree.
  const body = { path: '/tree/otter' };

  const rawEff = await rejectionOf(createWorktrees(throwingTailCtx(wrapping)).removeWorktree(body));
  const rawLeg = await rejectionOf(createWorktrees(throwingTailCtx()).removeWorktree(body));
  assert.equal(rawEff, sentinel, 'Effect arm must reject with the RAW error by identity');
  assert.equal(rawLeg, sentinel, 'legacy arm must reject with the RAW error by identity');
  assert.equal(rawEff, rawLeg, 'both arms must reject with the SAME error object');
  assert.equal(n, 1, 'one removeWorktree() on the Effect factory must discharge the runner');
  assert.notEqual(rawEff, NO_REJECTION, 'the Effect arm must not resolve');
  assert.notEqual(rawLeg, NO_REJECTION, 'the legacy arm must not resolve');
});

// =================== E. LIVE INGRESS BRIDGE (JOIN-ON-INTERRUPT) ===================
// The C-section quiesce pin stubs runRequest to Exit.fail(ApplicationQuiescingError)
// WITHOUT running the effect, so recorder.started() === null and the settler 503s.
// That cannot reproduce the ASYNC case the remove wrapper exists for: a real
// interrupt() landing on an already-admitted removal whose native core.removeWorktree
// settles only AFTER the interrupt. Here the recorder DID capture the Promise, so
// settleEffectRemoveRoute must JOIN it — never 503. A joined native REJECTION is
// written by onRejected (not the workflow fold) as the generic removal 500 dialect.

const runIngress = Effect.runPromiseWith(Context.empty());

interface Gate {
  /** How many times the real core method was actually invoked (0 ⇒ a true refusal). */
  invocations(): number;
  /** Whether the native operation has settled (flips strictly before the settler joins it). */
  settled(): boolean;
  /** Let the gated native operation run to completion. */
  release(): void;
}

// Replace ONE async core method with a gate: its returned Promise settles only after
// release(), modelling core.removeWorktree still in flight when the shutdown fiber
// interrupts the request. The remove dispatch's run thunk reads core.removeWorktree
// at invoke time, so overriding it after createHttp still takes effect; the recorder
// in http.ts captures THIS exact Promise, so `settled` flipping before the response
// resolves witnesses that the settler joined the native op rather than 503-ing.
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

function startLiveBoard(t: TestContext): Promise<LiveBoard> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home', runControlDetached });
  const rootScope = Scope.makeUnsafe('sequential');
  const probe = http.createServer();
  return new Promise<LiveBoard>((resolve, reject) => {
    probe.once('error', reject);
    probe.listen(0, '127.0.0.1', () => {
      const port = (probe.address() as AddressInfo).port;
      probe.close(() => {
        runIngress(makeIngressSupervisor(Context.empty(), rootScope)).then((supervisor) => {
          const handle = createHttp(core, { port, token: null as unknown as string, lan: null });
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

test('live bridge: interrupting an in-flight remove joins the started write (success AND rejection), never 503', async (t) => {
  // (a) Admitted, in flight, then interrupted → JOIN the TRUE remove DATA result
  // (a stubbed 200 removed) — never 503, never 500.
  const board = await startLiveBoard(t);
  const successWire = {
    status: 200,
    body: { ok: true, removed: true, branch_deleted: false, rows_purged: 0, path: '/tree/otter' },
  };
  board.core.removeWorktree = () => Promise.resolve(successWire);
  const gate = gateAsyncMethod(board.core, 'removeWorktree');

  const reqP = rawPost(board.port, '/api/worktrees/remove', { path: '/tree/otter', force: true });
  let responded = false;
  void reqP.then(() => {
    responded = true;
  });

  await waitFor(() => gate.invocations() === 1, 'removeWorktree invoked');
  assert.equal(board.supervisor.activeCount, 1, 'the request fiber is in flight');
  assert.equal(gate.settled(), false, 'the native remove has not settled yet');

  board.supervisor.interrupt();
  assert.equal(board.supervisor.state, 'quiescing', 'interrupt() quiesces admission');

  // The interrupt must NOT collapse to 503: the write already started, so the settler
  // JOINs it. The fiber's Exit resolves (activeCount → 0) but the response stays
  // pending on the still-gated native Promise.
  await waitFor(() => board.supervisor.activeCount === 0, 'the interrupted fiber settled');
  await Bun.sleep(20);
  assert.equal(responded, false, 'response must join the started write, not resolve to 503');
  assert.equal(gate.settled(), false, 'the joined remove is still gated');

  gate.release();
  const res = await within(reqP, 'joined remove success');
  assert.equal(gate.settled(), true, 'removeWorktree settled before the response resolved');
  assert.equal(res.status, 200, 'the TRUE remove DATA result — not 503, not 500');
  assert.equal(res.body, JSON.stringify(successWire.body));
  assert.notEqual(res.body, SHUTDOWN_503, 'not the shutting-down refusal');
  assert.notEqual(res.body, REMOVE_500, 'not the removal 500 fold');
  assert.notEqual(res.body, CONTROL_DEFECT_500, 'not CONTROL_DEFECT');

  // (b) Rejection-after-start: the native then REJECTS. The fiber is already
  // interrupted, so the workflow fold is not the writer — onRejected must render the
  // generic removal 500 dialect + log the removal line.
  const rejectBoard = await startLiveBoard(t);
  const boom = new Error('remove JOIN reject sentinel');
  rejectBoard.core.removeWorktree = () => Promise.reject(boom);
  const rejectGate = gateAsyncMethod(rejectBoard.core, 'removeWorktree');

  const rejectP = rawPost(rejectBoard.port, '/api/worktrees/remove', {
    path: '/tree/otter',
    force: true,
  });
  let rejectResponded = false;
  void rejectP.then(() => {
    rejectResponded = true;
  });

  await waitFor(() => rejectGate.invocations() === 1, 'rejecting removeWorktree invoked');
  rejectBoard.supervisor.interrupt();
  await waitFor(() => rejectBoard.supervisor.activeCount === 0, 'the rejecting fiber settled');
  await Bun.sleep(20);
  assert.equal(rejectResponded, false, 'rejection arm must JOIN, not 503');
  assert.equal(rejectGate.settled(), false, 'the joined rejection is still gated');

  rejectGate.release();
  const rejected = await withErrorLog(() => within(rejectP, 'joined remove rejection'));
  assert.equal(rejectGate.settled(), true, 'the joined rejection settled before the response');
  assert.equal(rejected.result.status, 500, 'joined rejection → 500, never 503');
  assert.equal(rejected.result.body, REMOVE_500, 'onRejected → the generic removal dialect');
  assert.notEqual(rejected.result.body, PREFLIGHT_500, "never preflight's distinct 500");
  assert.notEqual(rejected.result.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.notEqual(
    rejected.result.body,
    SHUTDOWN_503,
    'a joined rejection is not a quiesce refusal',
  );
  // Interrupt racing a native rejection may log the prefix twice (workflow fold
  // continuation + onRejected) — same as settleControlAsyncRoute / preflight. Bytes
  // stay single; every removal line must carry the RAW boom by identity.
  const joinLogs = removeLogs(rejected.errors);
  assert.ok(joinLogs.length >= 1 && joinLogs.length <= 2, `removal log count: ${joinLogs.length}`);
  for (const line of joinLogs) {
    assert.deepEqual(line, [REMOVE_LOG, boom], 'joined rejection logs the RAW error by identity');
  }
});
