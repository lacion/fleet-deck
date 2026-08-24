// http-workflow-worktrees.test.ts — the focused fixture for the P9.2 Slice 1
// WORKTREES READ route group: the single fleet-wide inspector snapshot
// GET /api/worktrees. It pins the CONVENTION in src/daemon/app/http-workflows/
// worktrees.ts three ways, exactly as the pilot suite (http-workflow-health-
// state.test.ts) and the control suite do for their groups:
//
//   A. ISOLATION — worktreesSnapshotWorkflow and the Exit → outcome mapper in pure
//      isolation with capability fakes: the success arm relays the snapshot
//      verbatim; a promise REJECTION is folded INSIDE the workflow to the soft body
//      { ok: true, worktrees: [] } (with onError logging the frozen inspector line),
//      surfacing as a SUCCESS Exit — never an Effect failure; lazy construction; and
//      the (structurally unreachable, core.worktrees is async) synchronous throw
//      that dies to the settler's never-500 defect arm.
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires worktreesSnapshot to the
//      live ingress bridge) answers GET /api/worktrees with the frozen soft-read
//      snapshot bytes on a real socket, proving the group is wired end-to-end.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler for the snapshot, that a core rejection fails SOFT to 200
//      { ok:true, worktrees:[] } + the inspector log on BOTH paths (byte- AND
//      log-identical), that a quiescing ingress REPLAYS the read (200 snapshot) —
//      the INVERTED policy vs the mutating control group, which refuses with 503,
//      because replaying a READ writes nothing — and that a workflow defect STILL
//      renders the fail-soft 200 body (DANGER §4.7 never-500), never the 500 the
//      mutating group emits.
//
// Every wire byte here is deterministic: an idle :memory: core has no remembered
// worktree spawns, so the real snapshot is { ok:true, worktrees:[] } — identical to
// the fail-soft body — and the SUCCESS-vs-FOLD distinction is carried by the log
// (the success arm logs nothing; the fold logs the inspector line). Section A pins
// the verbatim relay of a NON-empty snapshot with a capability fake.

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
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
} from '../../src/daemon/app/http-workflows/worktrees.ts';
import { repoPreflightWorkflow } from '../../src/daemon/app/http-workflows/repos.ts';
import { heldSettleWorkflow } from '../../src/daemon/app/http-workflows/held.ts';

import { startDaemon } from '../helpers/daemon.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// suite only exercises the worktrees group, but installEffectRoutes needs the whole
// port wired, so every group's real builders are folded in here unchanged (the
// non-worktrees groups are never reached in this suite — each has its own focused
// suite: control, health-state, paste, settings-command-mail-cleanup).
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

// The frozen fail-soft / empty-snapshot wire — the soft body the fold renders AND
// the real snapshot an idle core produces (no remembered spawns → []).
const SOFT_BODY = '{"ok":true,"worktrees":[]}';
// The frozen inspector log prefix, shared by the legacy `.catch`, the workflow's
// onError, and the settler's never-500 defect arm.
const INSPECTOR_LOG = 'fleetd worktree inspector error:';

// Capture console.error for the duration of one async request. The onError fold (or
// the settler's defect arm) logs BEFORE json() writes the response, and rawFull
// resolves only after the response ends, so a capture around the awaited request
// sees every inspector line the path emitted.
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

// ============================ A. ISOLATION ============================

test('worktreesSnapshotWorkflow relays a resolved snapshot verbatim', async () => {
  // A rich snapshot — proves the success arm relays the REAL inspector output
  // untouched, not a normalized/empty body. (An in-memory core is empty, so this
  // verbatim-relay guarantee can only be pinned with a capability fake.)
  const snapshot = {
    ok: true,
    worktrees: [{ repo: 'acme', branch: 'main', verdict: 'safe', ahead: 0, behind: 0 }],
  };
  let runCalls = 0;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreesSnapshotWorkflow({
      run: () => {
        runCalls += 1;
        return Promise.resolve(snapshot);
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'success');
  assert.equal(outcome.kind === 'success' ? outcome.value : null, snapshot); // same reference
  assert.equal(runCalls, 1);
  assert.equal(errCalls, 0);
});

test('worktreesSnapshotWorkflow folds a promise rejection into the soft read body and logs via onError', async () => {
  const boom = new Error('one broken worktree repository');
  let seen: unknown;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreesSnapshotWorkflow({
      run: () => Promise.reject(boom),
      onError: (err) => {
        errCalls += 1;
        seen = err;
      },
    }),
  );
  // The rejection is caught INSIDE the workflow and relayed as the SUCCESS wire the
  // legacy `.catch` wrote — never an Effect failure — while onError logs it. This is
  // the DANGER §4.7 never-500 read: even a total inspector failure is a 200.
  assert.deepEqual(mapEffectRouteExit(exit), {
    kind: 'success',
    value: { ok: true, worktrees: [] },
  });
  assert.equal(errCalls, 1);
  assert.equal(seen, boom); // identity preserved through the fold
});

test('worktreesSnapshotWorkflow builds lazily — constructing the Effect runs no core call', () => {
  let runCalls = 0;
  // Building the workflow must touch no capability: the inspector fan-out happens
  // only when the Effect runs (so a quiescing ingress that never runs it does no
  // work), exactly like every other converted route.
  worktreesSnapshotWorkflow({
    run: () => {
      runCalls += 1;
      return Promise.resolve({ ok: true, worktrees: [] });
    },
    onError: () => {},
  });
  assert.equal(runCalls, 0);
});

test('worktreesSnapshotWorkflow turns a synchronous throw into a die (never-500 defect arm, structurally unreachable)', async () => {
  const boom = new Error('threw while starting the core promise');
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    worktreesSnapshotWorkflow({
      run: () => {
        throw boom;
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  // A sync throw escapes the promise fold (there is no promise yet); Effect.sync
  // turns it into a die → the transport's defect arm. This arm is STRUCTURALLY
  // UNREACHABLE in production (core.worktrees() always returns a promise), but when
  // it maps to a defect the soft-read settler STILL answers 200 (never a 500) — see
  // the defect wire test in section C. onError never runs on this arm (no rejection
  // was folded), so the settler's own inspector log covers it.
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
  assert.equal(errCalls, 0);
});

// ============================ B. REAL DAEMON WIRE ============================

test('the wired daemon answers GET /api/worktrees with the frozen soft-read snapshot bytes', async () => {
  const daemon = await startDaemon();
  try {
    const r = await fetch(`${daemon.baseUrl}/api/worktrees`, {
      headers: { authorization: `Bearer ${daemon.token}` },
    });
    const text = await r.text();
    // A fresh daemon home remembers no worktree spawns → the empty snapshot.
    assert.equal(r.status, 200, 'worktrees → 200');
    assert.equal(text, SOFT_BODY, 'worktrees → frozen empty snapshot');
    assert.equal(r.headers.get('content-type'), 'application/json');
    assert.equal(r.headers.get('x-content-type-options'), 'nosniff');
    const cl = r.headers.get('content-length');
    if (cl !== null) assert.equal(cl, String(Buffer.byteLength(text)));
    const parsed = JSON.parse(text) as Record<string, unknown>;
    assert.equal(parsed['ok'], true, 'worktrees dialect (ok:true), not the 404 fall-through');
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

// One request over a real loopback socket, capturing status + headers + body so the
// pinnable headers (content-type, content-length, x-content-type-options) can be
// asserted exactly. Never rejects on a non-2xx status.
function rawFull(
  port: number,
  {
    method = 'GET',
    path: reqPath = '/',
    headers = {},
  }: {
    method?: string;
    path?: string;
    headers?: Record<string, string>;
  } = {},
): Promise<RawResponse> {
  return new Promise<RawResponse>((resolve, reject) => {
    const req = http.request({ host: '127.0.0.1', port, path: reqPath, method, headers }, (res) => {
      let received = '';
      res.on('data', (d: Buffer) => {
        received += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, headers: res.headers, body: received });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    req.end();
  });
}

// The in-process harness (mirrors the control/pilot suites): an idle :memory: core
// behind createHttp, bound on a real loopback port (bind a throwaway probe first to
// learn a free port, then hand createHttp that port). effectRoutes starts null — the
// legacy path — and the test installs the bridge when it wants. token:null + plain
// loopback authorizes the read GET (requireToken defaults off), so the request
// reaches the route handler rather than a 401 wall. The core is returned so a test
// can override core.worktrees to force the fail-soft arm.
function startBoard(t: TestContext): Promise<BoardHandle> {
  const db = openDb(':memory:');
  const core = createCore(db, { port: 0, home: '/daemon-home', runControlDetached });
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

// The snapshot body carries no clock field, so the two captures are strictly
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

test('workflow dispatch is byte-identical to the legacy handler for the worktrees snapshot', async (t) => {
  const board = await startBoard(t);

  // effectRoutes null ⇒ the legacy handler answers. An idle core has no spawns → the
  // success arm renders the real (empty) snapshot and logs NOTHING.
  const legacyRun = await withErrorLog(() => rawFull(board.port, { path: '/api/worktrees' }));
  const legacy = legacyRun.result;
  assert.equal(legacy.status, 200, 'legacy worktrees → 200');
  assert.equal(legacy.body, SOFT_BODY, 'legacy worktrees → empty snapshot');
  assert.deepEqual(legacyRun.errors, [], 'legacy success arm logs nothing');

  // Wire a FAITHFUL success bridge (runs the real workflow Effect through
  // Effect.runPromiseExit — the same Exit the ingress runtime produces).
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });

  const workflowRun = await withErrorLog(() => rawFull(board.port, { path: '/api/worktrees' }));
  assertByteIdentical(workflowRun.result, legacy, 'worktrees snapshot');
  assert.deepEqual(workflowRun.errors, [], 'workflow success arm logs nothing');
});

test('a core rejection fails soft to 200 { ok:true, worktrees:[] } + the inspector log on BOTH paths, byte- and log-identical', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('inspector fan-out rejected');
  // Force the fail-soft arm on both paths: the workflow's run capability and the
  // legacy handler both call this same rejecting core.worktrees.
  board.core.worktrees = () => Promise.reject(boom);

  // Legacy path (effectRoutes null): the `.catch` folds to the soft body + logs.
  const legacyRun = await withErrorLog(() => rawFull(board.port, { path: '/api/worktrees' }));
  const legacy = legacyRun.result;
  assert.equal(legacy.status, 200, 'legacy fail-soft → 200 (NEVER 500)');
  assert.equal(legacy.body, SOFT_BODY, 'legacy fail-soft → soft body');
  assert.deepEqual(
    legacyRun.errors,
    [[INSPECTOR_LOG, boom]],
    'legacy logs the inspector line once',
  );

  // Effect path: the workflow folds the same rejection to the same soft body, and
  // its onError logs the identical line — the settler answers 200, never a 500.
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
  const workflowRun = await withErrorLog(() => rawFull(board.port, { path: '/api/worktrees' }));
  assertByteIdentical(workflowRun.result, legacy, 'worktrees fail-soft');
  // Byte-identical wire AND log-identical: same prefix, same error by identity.
  assert.deepEqual(
    workflowRun.errors,
    [[INSPECTOR_LOG, boom]],
    'workflow logs the identical inspector line once',
  );
});

test('a quiescing ingress REPLAYS the read (200 snapshot) — NOT the mutating 503 refusal', async (t) => {
  const board = await startBoard(t);

  // The mutating control group refuses a quiescing write with 503 (a replay would
  // perform the refused write). A READ is the INVERTED policy: replaying it writes
  // nothing, so the soft-read settler falls back to the legacy handler and answers
  // the 200 snapshot the reader expected, exactly as before shutdown.
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const quiesced = await rawFull(board.port, { path: '/api/worktrees' });
  assert.equal(quiesced.status, 200, 'quiesce → 200 read replay, not 503');
  assert.notEqual(quiesced.status, 503, 'a READ must NEVER take the mutating 503 refusal');
  assert.equal(quiesced.body, SOFT_BODY, 'quiesce → the empty snapshot the legacy read produces');
  assert.equal(quiesced.headers['content-type'], 'application/json');
  assert.equal(quiesced.headers['x-content-type-options'], 'nosniff');
});

test('a workflow defect STILL renders the fail-soft 200 body — never the mutating 500', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('unexpected inspector fault');

  // A die (the structurally-unreachable sync-throw arm from section A, or any
  // unexpected fault) must NOT surface as the control group's 500 {"err":"internal"}
  // nor the GET snapshot 500 {} — DANGER §4.7 pins EVERY worktrees arm at 200. The
  // never-500 settler renders the soft body and logs the defect with the frozen
  // inspector line so a real regression is not swallowed silently.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(boom)),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await withErrorLog(() => rawFull(board.port, { path: '/api/worktrees' }));
  assert.equal(defected.result.status, 200, 'defect → 200 soft body, NEVER 500');
  assert.equal(defected.result.body, SOFT_BODY, 'defect → the fail-soft snapshot');
  assert.equal(defected.result.headers['content-type'], 'application/json');
  assert.equal(defected.result.headers['x-content-type-options'], 'nosniff');
  assert.deepEqual(
    defected.errors,
    [[INSPECTOR_LOG, boom]],
    'the settler logs the defect with the frozen inspector line',
  );
});
