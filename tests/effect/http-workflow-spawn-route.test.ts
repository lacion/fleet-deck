// http-workflow-spawn-route.test.ts — the focused fixture for P9.1 SLICE 6a: POST
// /api/spawn brought under the P6.4 transport WITHOUT converting spawn's core (that
// is Slice 6b). spawn is the SEVENTH port consumer, so this suite folds the whole
// HttpEffectRoutes port into ALL_ROUTE_BUILDERS (installEffectRoutes needs every
// group wired) but exercises ONLY the new spawn route.
//
// spawn is the ONE control POST whose failure body is COMPUTED from the escaped
// error, not static: the legacy .catch answered 500 {ok:false, reason:
// spawnFailureReason(err)}, where spawnFailureReason applies the git-hardening pass
// (scrubUrlCredentials + redactDiagnosticText + one-line + truncation) so a
// token-bearing clone URL never reaches the wire. Design D6 makes that redaction
// CONTRACTUAL. So spawn cannot ride settleEffectAsyncMutatingRoute (whose defect
// body is a static value it cannot vary per error) and cannot ride the CONTROL_DEFECT
// {"err":"internal"} arm; it owns a dedicated settler (settleEffectSpawnRoute) and a
// dedicated workflow (spawnRouteWorkflow) with ONE divergence from controlAsync:
//
//   THE NON-FOLD. controlAsyncWorkflow FOLDS a promise rejection INTO a success wire
//   (500 {ok:false,reason:'internal'}). spawnRouteWorkflow must NOT: it awaits the
//   RAW core promise under Effect.promise, so a rejection becomes a DIE, and the
//   settler renders spawnFailureReason(defect) on its defect / joined-rejection arms.
//
// The suite pins the CONVENTION the same three ways the pilot (health-state) and the
// control group do:
//
//   A. ISOLATION — spawnRouteWorkflow + spawnFailureReason in pure isolation with
//      capability fakes: a resolved control result relayed verbatim (incl. a
//      maintenance-gate 503 SUCCESS wire), the NON-FOLD (a promise rejection dies
//      carrying the RAW error, NOT a folded 500-internal wire), a synchronous throw
//      dying with the raw error, lazy construction, and the D6 redaction contract
//      spawnFailureReason enforces.
//   B. REAL DAEMON WIRE — a real subprocess daemon (program.ts wires spawnRoute into
//      the live ingress bridge) answers POST /api/spawn on a real socket with the
//      frozen envelope, proving the route is served end-to-end.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler: the 202/4xx success relay, the transport-wall 400 that
//      precedes BOTH paths, the redacted 500 dialect, the TWO distinct 503 sources
//      (transport-quiesce refusal vs a maintenance-gate SUCCESS wire), and the Exit
//      defect arm.
//   D. LIVE INGRESS BRIDGE (JOIN-ON-INTERRUPT) — the REAL LiveIngressSupervisor as
//      runRequest (the object program.ts installs) drives an actual interrupt against
//      a gated core.spawn: an interrupt AFTER the native started JOINs it and answers
//      its TRUE result (never 503); a quiesce BEFORE admission is the ONLY 503 case
//      and core.spawn is never called. A second live-ingress case drives a REAL
//      launch-time throw through the UNSTUBBED converted spawn composition (core
//      with runControlDetached, no core.spawn stub) and pins D6: HTTP 500
//      {"ok":false,"reason":"<thrown message>"} plus the 'fleetd spawn error:' log,
//      never {"err":"internal"}.
//
// §7 BINDING PIN NOTE: the three repo-mode validation 400s (worktree-in-repo,
// branch-required, branch_mode-invalid) each revert their plan claim to
// restoreStatus. Those live in tests/plan-claim-compensation.test.ts (the plan-claim
// compensation is what those 400s exercise); this suite pins the TRANSPORT contract.

import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import { tmpdir } from 'node:os';
import path from 'node:path';

import * as Cause from 'effect/Cause';
import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { createHttp } from '../../src/daemon/http.ts';
import { mapEffectRouteExit } from '../../src/daemon/http-policy.ts';
import { spawnFailureReason } from '../../src/daemon/spawns.ts';
import { ApplicationQuiescingError } from '../../src/daemon/app/errors.ts';
import type { IngressSupervisorService } from '../../src/daemon/app/services/ingress-supervisor.ts';
import { makeIngressSupervisor } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
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
// suite exercises only the spawn route, but installEffectRoutes needs the whole port
// wired, so every group's real builders are folded in unchanged (the non-spawn
// groups are never reached here — each has its own focused suite).
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
  // Production held runner (untracked, squashes on defect) — not a runPromiseExit
  // stub; see http-workflow-control.test.ts for the full rationale (finding 3).
  runHeld: runControlDetached,
} as const;

// The exact SUCCESS-wire 503 the spawn maintenance gate emits when
// maintenancePhase !== 'open' (spawns.ts:4895). It is a NORMAL control result — a
// resolved promise, relayed verbatim — and MUST NOT be confused with the transport
// quiesce refusal 503 {"ok":false,"reason":"shutting-down"} (which the settler emits
// when the workflow never ran). The two share a status code and nothing else.
const MAINTENANCE_GATE_503 = {
  status: 503,
  body: { ok: false, reason: 'daemon is shutting down; spawn maintenance is quiescing' },
} as const;

// A clone-failure message carrying a token-bearing remote URL — the exact shape the
// D6 redaction exists to catch. spawnFailureReason must scrub the credential before
// it reaches any wire; the literal token must never survive.
const TOKEN_SECRET = 'ghs_ThisIsAFakeCloneTokenABCDEF0123456789';
const TOKEN_URL_ERROR = new Error(
  `fatal: could not read from remote https://x-access-token:${TOKEN_SECRET}@github.com/o/private.git`,
);

// ============================ A. ISOLATION ============================

test('spawnRouteWorkflow relays a resolved control result verbatim (202 provisioning pass-through)', async () => {
  const resolved = { status: 202, body: { ok: true, id: 'sp_1', provisioning: true } };
  let runCalls = 0;
  const exit = await Effect.runPromiseExit(
    spawnRouteWorkflow({
      run: () => {
        runCalls += 1;
        return Promise.resolve(resolved);
      },
    }),
  );
  assert.deepEqual(mapEffectRouteExit(exit), { kind: 'success', value: resolved });
  assert.equal(runCalls, 1);
});

test('spawnRouteWorkflow relays the maintenance-gate 503 as a SUCCESS wire (not a transport refusal)', async () => {
  // ownedSpawn's shuttingDown() gate resolves this 503 as a normal control result;
  // the workflow relays it verbatim. It is a SUCCESS Exit, never a quiesce classification.
  const exit = await Effect.runPromiseExit(
    spawnRouteWorkflow({ run: () => Promise.resolve(MAINTENANCE_GATE_503) }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'success', 'the maintenance-gate 503 is a SUCCESS wire');
  assert.deepEqual(outcome.kind === 'success' ? outcome.value : null, MAINTENANCE_GATE_503);
});

test('spawnRouteWorkflow does NOT fold a promise rejection — it dies carrying the RAW error', async () => {
  // THE ONE DIVERGENCE FROM controlAsyncWorkflow. controlAsync would fold this into a
  // SUCCESS wire { status: 500, body: { ok: false, reason: 'internal' } }; spawn must
  // NOT — the rejection travels the DEFECT channel unchanged so the settler can render
  // the redacted spawnFailureReason. A fold would erase the reason (D6).
  const boom = new Error('core promise rejected while cloning');
  const exit = await Effect.runPromiseExit(spawnRouteWorkflow({ run: () => Promise.reject(boom) }));
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect', 'a rejection is a die, NOT a folded 500-internal wire');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom, 'the RAW error, unwrapped');
  // Prove the NON-FOLD explicitly: the workflow never produced controlAsync's folded wire.
  assert.notDeepEqual(outcome, {
    kind: 'success',
    value: { status: 500, body: { ok: false, reason: 'internal' } },
  });
});

test('spawnRouteWorkflow turns a synchronous throw into a die carrying the raw error', async () => {
  // The never-started-defect edge: a throw while CONSTRUCTING the promise (ownedSpawn
  // never does this — runMaintenance turns a sync throw into a rejected Promise) still
  // dies under Effect.sync, and the settler emits spawnFailureReason(defect), NOT
  // {"err":"internal"}.
  const boom = new Error('threw before returning the spawn promise');
  const exit = await Effect.runPromiseExit(
    spawnRouteWorkflow({
      run: () => {
        throw boom;
      },
    }),
  );
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
});

test('spawnRouteWorkflow builds lazily — constructing the Effect runs no core call', () => {
  let runCalls = 0;
  spawnRouteWorkflow({
    run: () => {
      runCalls += 1;
      return Promise.resolve({ status: 202 });
    },
  });
  assert.equal(
    runCalls,
    0,
    'the core spawn happens only when the Effect runs (never for a refusal)',
  );
});

test('spawnFailureReason redacts a token-bearing clone URL (the D6 contract the settler relies on)', () => {
  // This is the whole reason spawn owns a bespoke settler: the 500 body is COMPUTED
  // from the escaped error through the git-hardening pass, so a credential in git
  // stderr is scrubbed before it reaches the wire.
  const reason = spawnFailureReason(TOKEN_URL_ERROR);
  assert.ok(!reason.includes(TOKEN_SECRET), 'the raw token must NEVER reach the reason');
  assert.ok(!reason.includes('x-access-token'), 'the userinfo is scrubbed whole');
  assert.ok(reason.includes('[redacted]'), 'the credential collapses to the redaction marker');
  assert.ok(!reason.includes('\n'), 'the reason is one line');
  assert.ok(reason.length > 0, 'a non-empty verdict, never a bare fallback here');
});

// ============================ B. REAL DAEMON WIRE ============================

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

test('the wired daemon serves POST /api/spawn with the frozen control envelope', async () => {
  // FLEETDECK_SPAWN=off makes the dispatch DETERMINISTIC and side-effect-free: the
  // capability gate refuses before any card/worktree/tmux work, so an object body
  // resolves a frozen 400 instead of really launching an agent on this box's tmux.
  const daemon = await startDaemon({ env: { FLEETDECK_SPAWN: 'off' } });
  const post = async (body: string) => {
    const r = await fetch(`${daemon.baseUrl}/api/spawn`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${daemon.token}`,
      },
      body,
    });
    return { status: r.status, headers: r.headers, text: await r.text() };
  };
  try {
    // A non-object body hits the transport-wall 400 that precedes BOTH the effect
    // dispatch and the legacy handler — daemon-independent and frozen. Proves the
    // daemon serves the route with the pinned envelope.
    const wall = await post('[]');
    assert.equal(wall.status, 400, 'non-object body → transport-wall 400');
    assert.equal(wall.headers.get('content-type'), 'application/json');
    assert.equal(wall.headers.get('x-content-type-options'), 'nosniff');
    assert.deepEqual(JSON.parse(wall.text), {
      ok: false,
      reason: 'spawn body must be a JSON object',
    });

    // An object body reaches DISPATCH through the installed spawnRoute bridge; with
    // FLEETDECK_SPAWN=off the capability gate resolves a frozen 400 wire — proving the
    // route ran end-to-end through the effect transport, control dialect (ok:false),
    // not the router {"err":"nope"} fall-through, and never a real spawn.
    const dispatched = await post('{}');
    assert.equal(dispatched.headers.get('content-type'), 'application/json');
    assert.equal(dispatched.headers.get('x-content-type-options'), 'nosniff');
    const parsed = JSON.parse(dispatched.text) as Record<string, unknown>;
    assert.equal(parsed['ok'], false, 'the route ran (control dialect), not the 404 fall-through');
    assert.equal(dispatched.status, 400, 'spawn refused (FLEETDECK_SPAWN=off) → 400');
    assert.equal(parsed['reason'], 'spawning unavailable: disabled (FLEETDECK_SPAWN=off)');
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

// The in-process harness (mirrors the control pilot): an idle :memory: core behind
// createHttp, bound on a real loopback port. effectRoutes starts null — the legacy
// path — and the test installs the bridge when it wants. token:null + plain loopback
// authorizes the spawn POST (requireToken defaults off). `core` is handed back so a
// test can override core.spawn before capturing.
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

// Replace core.spawn with a fixed implementation (the core is UNCHANGED this slice,
// so faking the assembled control result is the honest way to exercise every settler
// arm deterministically). The route's run thunk reads core.spawn at invoke time, so
// overriding after createHttp still takes effect. Returns a call counter.
function overrideSpawn(
  core: ReturnType<typeof createCore>,
  impl: () => Promise<{ status: number; body?: unknown }>,
): { calls: () => number } {
  const holder = core as unknown as Record<string, (...a: unknown[]) => Promise<unknown>>;
  let calls = 0;
  holder['spawn'] = (...a: unknown[]): Promise<unknown> => {
    calls += 1;
    return impl.apply(null, a as []);
  };
  return { calls: () => calls };
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

function installFaithful(board: BoardHandle): void {
  // A FAITHFUL success bridge: runs the real workflow Effect through
  // Effect.runPromiseExit — the same Exit the ingress runtime produces.
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
}

function postSpawn(port: number, body: string): Promise<RawResponse> {
  return rawFull(port, { method: 'POST', path: '/api/spawn', body });
}

test('spawn dispatch is byte-identical to legacy for a resolved 4xx control result', async (t) => {
  const board = await startBoard(t);
  // A frozen dispatch-level control result (a validation 4xx an idle core would
  // assemble) — no mutation, so legacy-then-workflow on the same core agrees byte-for-byte.
  overrideSpawn(board.core, () =>
    Promise.resolve({
      status: 400,
      body: { ok: false, reason: "kind must be 'claude' or 'shell'" },
    }),
  );

  const legacy = await postSpawn(board.port, '{"kind":"nope"}');
  assert.equal(legacy.status, 400, 'legacy assembled 4xx');
  assert.equal((JSON.parse(legacy.body) as { ok?: unknown }).ok, false);

  installFaithful(board);
  const workflow = await postSpawn(board.port, '{"kind":"nope"}');
  assertByteIdentical(workflow, legacy, 'resolved 4xx relay');
});

test('the transport-wall 400 precedes BOTH paths and never reaches dispatch', async (t) => {
  const board = await startBoard(t);
  // A counter proves the validateSpawnRequest wall fires BEFORE the effect dispatch:
  // a non-object body must 400 without ever calling core.spawn, bridge or no bridge.
  const spy = overrideSpawn(board.core, () => Promise.resolve({ status: 202 }));

  const legacy = await postSpawn(board.port, '[]');
  assert.equal(legacy.status, 400, 'legacy wall 400');
  assert.deepEqual(JSON.parse(legacy.body), {
    ok: false,
    reason: 'spawn body must be a JSON object',
  });

  installFaithful(board);
  const workflow = await postSpawn(board.port, '[]');
  assertByteIdentical(workflow, legacy, 'transport-wall 400');
  assert.equal(spy.calls(), 0, 'the wall precedes dispatch — core.spawn is NEVER called');
});

test('a spawn rejection reproduces the legacy 500 spawnFailureReason dialect — redacted, byte-identical', async (t) => {
  const board = await startBoard(t);
  // The core promise REJECTS with a token-bearing clone error. The legacy .catch and
  // the effect settler must BOTH answer 500 { ok:false, reason: spawnFailureReason(err) }
  // with the credential scrubbed — never {"err":"internal"}, never {"ok":false,"reason":"internal"}.
  overrideSpawn(board.core, () => Promise.reject(TOKEN_URL_ERROR));

  const legacy = await postSpawn(board.port, '{}');
  assert.equal(legacy.status, 500, 'legacy rejection → 500');
  const legacyBody = JSON.parse(legacy.body) as { ok?: unknown; reason?: unknown };
  assert.equal(legacyBody.ok, false);
  assert.equal(
    legacyBody.reason,
    spawnFailureReason(TOKEN_URL_ERROR),
    'the computed redacted reason',
  );
  assert.ok(!legacy.body.includes(TOKEN_SECRET), 'legacy body redacts the token');

  installFaithful(board);
  const workflow = await postSpawn(board.port, '{}');
  assertByteIdentical(workflow, legacy, 'redacted 500 dialect');
  // Prove the settler chose the spawn dialect, NOT a generic internal body.
  assert.ok(!workflow.body.includes(TOKEN_SECRET), 'workflow body redacts the token');
  assert.notEqual(workflow.body, '{"err":"internal"}', 'not the outer-catch defect body');
  assert.notEqual(workflow.body, '{"ok":false,"reason":"internal"}', 'not the controlAsync fold');
});

test('the TWO 503 sources are byte-distinct: a maintenance-gate SUCCESS wire vs a transport-quiesce refusal', async (t) => {
  const board = await startBoard(t);

  // (b) MAINTENANCE-GATE 503 — a NORMAL success wire ownedSpawn resolves when
  // maintenancePhase !== 'open'. Relayed verbatim through legacy AND the faithful bridge.
  overrideSpawn(board.core, () => Promise.resolve(MAINTENANCE_GATE_503));
  const legacyMaint = await postSpawn(board.port, '{}');
  assert.equal(legacyMaint.status, 503, 'maintenance-gate → 503');
  assert.deepEqual(JSON.parse(legacyMaint.body), MAINTENANCE_GATE_503.body);

  installFaithful(board);
  const workflowMaint = await postSpawn(board.port, '{}');
  assertByteIdentical(workflowMaint, legacyMaint, 'maintenance-gate 503 SUCCESS wire');
  assert.equal(
    workflowMaint.body,
    '{"ok":false,"reason":"daemon is shutting down; spawn maintenance is quiescing"}',
    'maintenance-gate frozen body',
  );

  // (a) TRANSPORT-QUIESCE 503 — a refusal: the bridge resolves runRequest to a failed
  // Exit WITHOUT running the workflow, so core.spawn is never called and the settler
  // emits the SHUTTING-DOWN body. Distinct bytes from (b).
  const spy = overrideSpawn(board.core, () => Promise.resolve(MAINTENANCE_GATE_503));
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });
  const quiesced = await postSpawn(board.port, '{}');
  assert.equal(quiesced.status, 503, 'transport-quiesce → 503');
  assert.equal(
    quiesced.body,
    '{"ok":false,"reason":"shutting-down"}',
    'the transport refusal body',
  );
  assert.equal(spy.calls(), 0, 'a refused admission never calls core.spawn');

  // The whole point: SAME status, DIFFERENT bytes. The refusal must never be mistaken
  // for the maintenance gate, and vice-versa.
  assert.notEqual(quiesced.body, workflowMaint.body, 'the two 503 bodies are byte-distinct');
});

test('an interrupts-only Exit before start takes the same 503 refusal', async (t) => {
  const board = await startBoard(t);
  const spy = overrideSpawn(board.core, () => Promise.resolve({ status: 202 }));
  // A fixed interrupts-only Exit (the sync-refusal shape): the recorder never captured
  // a native Promise, so the settler classifies quiesce and emits the refusal 503.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });
  const interrupted = await postSpawn(board.port, '{}');
  assert.equal(interrupted.status, 503);
  assert.equal(interrupted.body, '{"ok":false,"reason":"shutting-down"}');
  assert.equal(interrupted.headers['content-type'], 'application/json');
  assert.equal(interrupted.headers['x-content-type-options'], 'nosniff');
  assert.equal(spy.calls(), 0, 'a never-started interrupt never calls core.spawn');
});

test('a never-started workflow defect surfaces spawnFailureReason via the outer catch (NOT {"err":"internal"})', async (t) => {
  const board = await startBoard(t);
  // A die whose native never started (recorder.started() === null): the settler
  // re-throws the defect into its outer .catch → emitSpawnFailure → the spawn 500
  // dialect. D6: spawn NEVER answers the generic {"err":"internal"} the control
  // routes' CONTROL_DEFECT arm emits.
  const spy = overrideSpawn(board.core, () => Promise.resolve({ status: 202 }));
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(TOKEN_URL_ERROR)),
    ...ALL_ROUTE_BUILDERS,
  });
  const defected = await postSpawn(board.port, '{}');
  assert.equal(defected.status, 500);
  assert.deepEqual(JSON.parse(defected.body), {
    ok: false,
    reason: spawnFailureReason(TOKEN_URL_ERROR),
  });
  assert.ok(!defected.body.includes(TOKEN_SECRET), 'the outer-catch body is redacted too');
  assert.notEqual(defected.body, '{"err":"internal"}', 'spawn NEVER emits the generic defect body');
  assert.equal(spy.calls(), 0, 'this Exit.die models a fault where the native never started');
});

// =================== D. LIVE INGRESS BRIDGE (JOIN-ON-INTERRUPT) ===================
// The C-section interrupt test proves the never-started case with a fixed
// Exit.failCause(Cause.interrupt(1)). That fixed Exit CANNOT reproduce the arm the
// join-on-interrupt fix exists for: a real interrupt() landing on an ALREADY-admitted
// spawn whose native core Promise (core.spawn) settles only AFTER the interrupt. Here
// the recorder DID capture the Promise, so settleEffectSpawnRoute must JOIN it and
// answer its TRUE control result — never 503 — with closeClients waiting for res.done.
// This wires the REAL LiveIngressSupervisor as runRequest (the object program.ts
// installs) and drives an actual interrupt against a gated core.spawn.

const runIngress = Effect.runPromiseWith(Context.empty());

interface Gate {
  invocations(): number;
  settled(): boolean;
  release(): void;
}

// Replace core.spawn with a gate: its returned Promise settles only after release(),
// modelling core.spawn still in flight when the shutdown fiber interrupts the request.
// The recorder in http.ts captures THIS exact Promise, so `settled` flipping before
// the response resolves witnesses that the settler joined the native op rather than 503-ing.
function gateSpawn(core: unknown, result: { status: number; body?: unknown }): Gate {
  const holder = core as Record<string, (...args: unknown[]) => Promise<unknown>>;
  let invocations = 0;
  let settled = false;
  let open: () => void = () => undefined;
  const opened = new Promise<void>((resolve) => {
    open = resolve;
  });
  holder['spawn'] = (): Promise<unknown> => {
    invocations += 1;
    return opened.then(() => {
      settled = true;
      return result;
    });
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

type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

// Narrow test tmux adapter (same surface as plan-claim-compensation). Default
// launchOverride is a no-op; the D6 pin overrides it to throw at launch time.
function makeAdapter(overrides: Partial<CoreTmuxAdapter> = {}): CoreTmuxAdapter {
  const adapter = {
    spawnOverrideCmd: () => null,
    hasTmux: () => true,
    tmuxCapability: () => ({ available: true }),
    fleetServerAbsent: () => Promise.resolve(false),
    capturePane: () => Promise.resolve('ready'),
    pasteText: () => Promise.resolve(true),
    sendEnter: () => Promise.resolve(true),
    sendBringupEnter: () => Promise.resolve(true),
    killWindowVerified: () => Promise.resolve({ ok: true }),
    launchOverride: () => {
      /* unused by default */
    },
    ensureSession: () => Promise.resolve('fleetdeck-4711'),
    newWindow: () =>
      Promise.resolve({
        session: 'fleetdeck-4711',
        window: 'fd4711-test',
        window_id: '@1',
      }),
    sessionName: () => 'fleetdeck-4711',
    windowName: (_port: number, callsign: string) => `fd4711-${callsign}`,
    typeAndEnter: () => Promise.resolve(true),
    listScopedWindows: () => Promise.resolve([]),
    paneCurrentCommand: () => Promise.resolve(null),
    ...overrides,
  };
  return adapter as unknown as CoreTmuxAdapter;
}

// Wires the REAL supervisor (runRequest === supervisor.runPromiseExit, exactly as
// makeHttpServerOwner does in production) so interrupt()/quiesce() drive genuine
// fiber lifecycle, and hands `core` back so a test can gate core.spawn before firing.
// `runControlDetached` is always injected so core.spawn walks spawnEffect — the
// production dispatcher. Optional home/tmuxAdapter let a case drive the REAL
// converted spawn (no core.spawn stub) through launchOverride.
function startLiveBoard(
  t: TestContext,
  opts: { home?: string; tmuxAdapter?: CoreTmuxAdapter } = {},
): Promise<LiveBoard> {
  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 0,
    home: opts.home ?? '/daemon-home',
    runControlDetached,
    ...(opts.tmuxAdapter ? { tmuxAdapter: opts.tmuxAdapter } : {}),
  });
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

test('live bridge: interrupting an in-flight spawn JOINs the started core.spawn and answers its TRUE result, while a quiesce-before-start refuses with 503 and never calls spawn', async (t) => {
  // (a) + (b): admitted, in flight, then interrupted → JOIN, then the TRUE control
  // result (the gated 202 wire) — never 503, never 500.
  const board = await startLiveBoard(t);
  const TRUE_WIRE = { status: 202, body: { ok: true, id: 'sp_live', provisioning: true } };
  const gate = gateSpawn(board.core, TRUE_WIRE);

  const reqP = postSpawn(board.port, '{}');
  let responded = false;
  void reqP.then(() => {
    responded = true;
  });

  await waitFor(() => gate.invocations() === 1, 'core.spawn invoked');
  assert.equal(board.supervisor.activeCount, 1, 'the request fiber is in flight');
  assert.equal(gate.settled(), false, 'the native spawn has not settled yet');

  board.supervisor.interrupt();
  assert.equal(board.supervisor.state, 'quiescing', 'interrupt() quiesces admission');

  // The interrupt must NOT collapse to 503: the write already started, so the settler
  // JOINs it. The fiber's Exit resolves (activeCount → 0) but the response stays
  // pending on the still-gated native Promise.
  await waitFor(() => board.supervisor.activeCount === 0, 'the interrupted fiber settled');
  await Bun.sleep(20);
  assert.equal(responded, false, 'response must join the started spawn, not resolve to 503');
  assert.equal(gate.settled(), false, 'the joined spawn is still gated');

  gate.release();
  const res = await within(reqP, 'joined spawn response');
  assert.equal(gate.settled(), true, 'core.spawn settled before the response resolved');
  assert.equal(res.status, 202, 'the TRUE control result (202 provisioning) — not 503, not 500');
  assert.deepEqual(JSON.parse(res.body), TRUE_WIRE.body);
  assert.notEqual(
    res.body,
    '{"ok":false,"reason":"shutting-down"}',
    'not the shutting-down refusal',
  );
  assert.notEqual(res.body, '{"err":"internal"}', 'not the outer-catch defect');

  // (c) A quiesce BEFORE admission is the ONLY 503 case — core.spawn is never called.
  const refusalBoard = await startLiveBoard(t);
  const refusalGate = gateSpawn(refusalBoard.core, TRUE_WIRE);
  refusalBoard.supervisor.quiesce();
  const refused = await within(postSpawn(refusalBoard.port, '{}'), 'quiesce-before-start spawn');
  assert.equal(refused.status, 503, 'quiesce-before-start → 503');
  assert.equal(refused.body, '{"ok":false,"reason":"shutting-down"}');
  assert.equal(refused.headers['content-type'], 'application/json');
  assert.equal(refused.headers['x-content-type-options'], 'nosniff');
  assert.equal(refusalGate.invocations(), 0, 'a refused admission never invokes core.spawn');
});

test('live bridge: a real launch-time throw through unstubbed spawnEffect answers 500 spawnFailureReason (not {err:internal}) and logs fleetd spawn error:', async (t) => {
  // Finding 1 / D6: the production composition is
  //   launchOverride throw → Effect.promise DIE → runControlDetached squash →
  //   ownedSpawn reject → spawnRouteWorkflow DIE → settleEffectSpawnRoute →
  //   emitSpawnFailure → console.error('fleetd spawn error:', err) +
  //   json(res, 500, {ok:false, reason: spawnFailureReason(err)})
  // The C-section 500 pins the settler dialect with core.spawn STUBBED, so they
  // never enter spawnEffect. This case does not stub: LiveIngressSupervisor as
  // runRequest (program.ts), runControlDetached injected, launchOverride throws.
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-spawn-d6-http-'));
  const LAUNCH_BOOM = 'launch override boom';
  const board = await startLiveBoard(t, {
    home: cwd,
    tmuxAdapter: makeAdapter({
      spawnOverrideCmd: () => '/fake-spawn-override',
      launchOverride: () => {
        throw new Error(LAUNCH_BOOM);
      },
    }),
  });
  t.after(() => rmSync(cwd, { recursive: true, force: true }));

  const logged: unknown[][] = [];
  const originalError = console.error;
  console.error = (...args: unknown[]) => {
    logged.push(args);
  };
  let res: RawResponse;
  try {
    res = await postSpawn(board.port, JSON.stringify({ cwd, prompt: 'x' }));
  } finally {
    console.error = originalError;
  }

  assert.equal(res.status, 500, 'launch-time throw → HTTP 500');
  assert.equal(
    res.body,
    JSON.stringify({ ok: false, reason: LAUNCH_BOOM }),
    'wire is exactly 500 {ok:false, reason: <the thrown message>}',
  );
  assert.equal(res.body, `{"ok":false,"reason":"${LAUNCH_BOOM}"}`);
  assert.notEqual(res.body, '{"err":"internal"}', 'not the outer-catch CONTROL_DEFECT body');
  assert.notEqual(
    res.body,
    '{"ok":false,"reason":"internal"}',
    'not the controlAsync folded 500-internal wire',
  );
  const spawnLog = logged.find((args) => args[0] === 'fleetd spawn error:');
  assert.ok(spawnLog, "console.error('fleetd spawn error:', err) fired");
  assert.match(
    String(spawnLog[1]),
    /launch override boom/,
    'the log carries the thrown launch error, not a Cause-inspection TypeError',
  );
});
