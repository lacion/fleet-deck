// http-workflow-repos.test.ts — the focused fixture for the P9.2 Slice 2 REPOS
// PREFLIGHT route group: the pre-spawn git-access probe POST /api/repos/preflight.
// It pins the CONVENTION in src/daemon/app/http-workflows/repos.ts the same four
// ways the worktrees / control / pilot suites pin theirs, PLUS a CORE
// dispatcher-liveness pin for the degenerate zero-gate Effect core in repos.ts:
//
//   A. ISOLATION — repoPreflightWorkflow and the Exit → outcome mapper in pure
//      isolation with capability fakes: a resolved wire (200 clone, 409 git_access)
//      is relayed VERBATIM as the SUCCESS value — expected failures are DATA, so
//      E=never and mapEffectRouteExit grows no case; a promise REJECTION is folded
//      INSIDE the workflow to the DISTINCT 500 wire
//      {ok:false,reason:'Git access check failed internally'} (onError logging the
//      frozen preflight line), surfacing as SUCCESS — never an Effect failure and
//      NEVER controlAsync's {reason:'internal'}; lazy construction; and the
//      (structurally unreachable — preflightRepo is a sync dispatcher whose both
//      legs always return a Promise) synchronous throw that dies to the settler's
//      500 PREFLIGHT_DEFECT arm (never {err:'internal'}).
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires repoPreflight to the live
//      ingress bridge AND injects runControlDetached into the repos ctx) answers
//      POST /api/repos/preflight on a real socket with a deterministic 400 DATA
//      dialect, proving the group is wired end-to-end through BOTH the Effect core
//      and the Effect transport.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler for a 200 clone wire and a 409 git_access wire, that a core
//      rejection folds to the DISTINCT 500 preflight dialect + the preflight log on
//      BOTH paths (byte- AND log-identical), that a quiescing ingress REFUSES with
//      503 {ok:false,reason:'shutting-down'} (preflight is a MUTATING-family POST —
//      NOT a read replay), and that a workflow defect renders the SAME 500 preflight
//      dialect (NEVER controlAsync's {err:'internal'} / {reason:'internal'}, NEVER a
//      503) + the preflight log.
//   D. CORE DISPATCHER LIVENESS — the repos.ts degenerate dispatcher routes through
//      the injected runControlDetached when present (runner invoked exactly once)
//      and falls to the verbatim legacy body when absent, both producing the
//      identical wire. Driven with a network-free, env-independent 400 target
//      ({repo:''} → parseRepoInput rejects before any catalog/git/env lookup).
//   E. LIVE JOIN — admission succeeds, startOnce captures the native write, the
//      request fiber is interrupted → the settler JOINS (never 503). A joined
//      native REJECTION is rendered by onRejected as the DISTINCT preflight 500
//      dialect (never CONTROL_DEFECT / controlAsync internal / 503). Modeled on
//      the control JOIN pin (http-workflow-control.test.ts live bridge).
//
// Every wire byte here is deterministic and network-free: section A/C use capability
// / core stubs, section B's 400 (repo_org on a URL) throws inside resolveTarget
// BEFORE any catalog/git/env lookup, and section D's 400 (empty repo) is rejected by
// parseRepoInput before resolveTarget touches anything. The live JOIN in E gates a
// stubbed core.preflightRepo (no git). The transport 400 wall ({repo:1}) is pinned
// in C with runner/core spy 0 — it returns before logExec / the recorder.

import assert from 'node:assert/strict';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

import * as Context from 'effect/Context';
import * as Effect from 'effect/Effect';
import * as Exit from 'effect/Exit';
import * as Scope from 'effect/Scope';

import { openDb } from '../../src/daemon/db.ts';
import { createCore } from '../../src/daemon/derive.ts';
import { createRepos } from '../../src/daemon/repos.ts';
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
} from '../../src/daemon/app/http-workflows/worktrees.ts';
import { repoPreflightWorkflow } from '../../src/daemon/app/http-workflows/repos.ts';
import { heldSettleWorkflow } from '../../src/daemon/app/http-workflows/held.ts';

import { startDaemon } from '../helpers/daemon.ts';
import { postJson } from '../helpers/http.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

// Port growth: HttpEffectRoutes requires every converted group's builders. This
// suite only exercises the repos-preflight group, but installEffectRoutes needs the
// whole port wired, so every group's real builders are folded in here unchanged (the
// non-preflight groups are never reached in this suite — each has its own focused
// suite).
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

// The frozen DISTINCT preflight 500 wire — the fold body AND the settler defect body.
const PREFLIGHT_500 = '{"ok":false,"reason":"Git access check failed internally"}';
// The frozen preflight log prefix — shared by the legacy `.catch`, the workflow's
// onError, and the settler's defect arm.
const PREFLIGHT_LOG = 'fleetd repo preflight error:';
// The dialects the preflight route must NEVER collapse into.
const CONTROL_ASYNC_500 = '{"ok":false,"reason":"internal"}'; // controlAsync fold
const CONTROL_DEFECT_500 = '{"err":"internal"}'; // CONTROL_DEFECT arm
const SHUTDOWN_503 = '{"ok":false,"reason":"shutting-down"}';

// Every preflight request first emits one audit line via logExec
// (console.error('fleetd exec /api/repos/preflight …')) on BOTH the legacy and the
// effect path (it runs before the effectRoutes branch, so it is symmetric and moves
// no wire byte). The pins below therefore filter the capture to the PREFLIGHT_LOG
// error line — the slice-0 `logged.find` technique — rather than deep-equalling the
// whole capture, so the unrelated audit line is not mistaken for a preflight error.
function preflightLogs(errors: unknown[][]): unknown[][] {
  return errors.filter((a) => a[0] === PREFLIGHT_LOG);
}

// Capture console.error for the duration of one async request. onError (or the
// settler defect arm) logs BEFORE json() writes the response, and the raw request
// resolves only after the response ends, so a capture around the awaited request
// sees every preflight line the path emitted.
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

test('repoPreflightWorkflow relays a resolved 200 clone wire verbatim (success is DATA, E=never)', async () => {
  // A concrete clone-mode wire — proves the success arm relays the REAL core output
  // untouched (an in-memory core can only produce a 200 local or a 404, so the
  // verbatim-relay of a network-derived 200 clone can only be pinned with a fake).
  const wire = {
    status: 200,
    body: { ok: true, mode: 'clone', provider: 'github', transport: 'https' },
  };
  let runCalls = 0;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    repoPreflightWorkflow({
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

test('repoPreflightWorkflow relays a 409 git_access wire verbatim (expected failure is DATA, not an Effect error)', async () => {
  // The 409 git_access dialect is an EXPECTED outcome the core already encodes as a
  // { status, body } wire — it rides the success channel verbatim, never lifted into
  // a typed error, so the port's error channel stays `never`.
  const wire = {
    status: 409,
    body: {
      ok: false,
      reason: 'repository not found or no access',
      git_access: { code: 'not_found', title: 'Repository not found' },
    },
  };
  const exit = await Effect.runPromiseExit(
    repoPreflightWorkflow({ run: () => Promise.resolve(wire), onError: () => {} }),
  );
  assert.deepEqual(mapEffectRouteExit(exit), { kind: 'success', value: wire });
});

test('repoPreflightWorkflow folds a promise rejection into the DISTINCT preflight 500 wire and logs via onError', async () => {
  const boom = new Error('git subprocess exploded');
  let seen: unknown;
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    repoPreflightWorkflow({
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
  assert.deepEqual(value, {
    status: 500,
    body: { ok: false, reason: 'Git access check failed internally' },
  });
  assert.equal(errCalls, 1);
  assert.equal(seen, boom); // identity preserved through the fold
  // DISTINCT dialect guard: NOT controlAsync's {reason:'internal'} (DANGER §4.6).
  assert.notEqual(JSON.stringify(value?.body), CONTROL_ASYNC_500);
});

test('repoPreflightWorkflow builds lazily — constructing the Effect runs no core call', () => {
  let runCalls = 0;
  // Building the workflow must touch no capability: the git probe happens only when
  // the Effect runs (so a quiescing ingress that never runs it does no work).
  repoPreflightWorkflow({
    run: () => {
      runCalls += 1;
      return Promise.resolve({ status: 200 });
    },
    onError: () => {},
  });
  assert.equal(runCalls, 0);
});

test('repoPreflightWorkflow turns a synchronous throw into a die (500 defect arm, structurally unreachable)', async () => {
  const boom = new Error('threw while starting the core promise');
  let errCalls = 0;
  const exit = await Effect.runPromiseExit(
    repoPreflightWorkflow({
      run: () => {
        throw boom;
      },
      onError: () => {
        errCalls += 1;
      },
    }),
  );
  // A sync throw escapes the promise fold (there is no promise yet); Effect.sync
  // turns it into a die → the transport's defect arm (500 preflight dialect — see the
  // defect wire test in section C). This arm is STRUCTURALLY UNREACHABLE in
  // production: preflightRepo is a sync dispatcher (repos.ts) whose both legs
  // (runControlDetached(...) and preflightRepoLegacy) always return a Promise, so
  // Effect.sync cannot throw. DIE still renders PREFLIGHT_DEFECT, never
  // {err:'internal'}. onError never runs on this arm (no rejection was folded), so
  // the settler's own preflight log covers it.
  const outcome = mapEffectRouteExit(exit);
  assert.equal(outcome.kind, 'defect');
  assert.equal(outcome.kind === 'defect' ? outcome.defect : null, boom);
  assert.equal(errCalls, 0);
});

// ============================ B. REAL DAEMON WIRE ============================

test('the wired daemon answers POST /api/repos/preflight through the effect route (deterministic 400 DATA)', async () => {
  const daemon = await startDaemon();
  try {
    // repo_org on a URL target is a 400 DATA outcome the workflow relays verbatim; it
    // throws inside resolveTarget BEFORE any catalog / git / default-org lookup, so
    // the wire is deterministic AND env-independent (unlike the no-default-org 404,
    // which depends on FLEETDECK_DEFAULT_ORG / a Coder workspace). A fall-through
    // (route not wired) would be a 404/405, not this dialect — so a 400 with this
    // exact reason proves the group is wired end-to-end.
    const r = await postJson(
      `${daemon.baseUrl}/api/repos/preflight`,
      { repo: 'https://github.com/octocat/hello-world', repo_org: 'acme' },
      { token: daemon.token },
    );
    assert.equal(r.status, 400, 'preflight repo_org+URL → 400 DATA');
    assert.deepEqual(r.json, {
      ok: false,
      reason: 'repo_org applies only to a bare repo name',
    });
    // Not a fall-through and not the 500 defect dialect.
    assert.notEqual(r.text, PREFLIGHT_500, 'a DATA 400, not the 500 defect fold');
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

// The in-process harness (mirrors the control / worktrees suites): an idle :memory:
// core behind createHttp, bound on a real loopback port. effectRoutes starts null —
// the legacy path — and the test installs the bridge when it wants. token:null +
// plain loopback authorizes the POST (preflight is not token-gated), so the request
// reaches the route handler rather than a 401 wall. The core is returned so a test
// can override core.preflightRepo to force a specific wire / rejection.
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

// The preflight wires carry no clock field, so the two captures are strictly
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
// two DATA dialects (200 clone, 409 git_access) both ride the success channel, so
// this is the dual-path parity for every non-defect outcome.
for (const dialect of [
  {
    name: '200 clone',
    wire: {
      status: 200,
      body: { ok: true, mode: 'clone', provider: 'github', transport: 'https' },
    },
  },
  {
    name: '409 git_access',
    wire: {
      status: 409,
      body: {
        ok: false,
        reason: 'repository not found or no access',
        git_access: { code: 'not_found', title: 'Repository not found' },
      },
    },
  },
] as const) {
  test(`workflow dispatch is byte-identical to the legacy handler for a ${dialect.name} wire`, async (t) => {
    const board = await startBoard(t);
    board.core.preflightRepo = () => Promise.resolve(dialect.wire);

    // effectRoutes null ⇒ the legacy `.then(json)` handler answers — no log.
    const legacyRun = await withErrorLog(() =>
      rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' }),
    );
    const legacy = legacyRun.result;
    assert.equal(legacy.status, dialect.wire.status, `legacy ${dialect.name} status`);
    assert.deepEqual(
      preflightLogs(legacyRun.errors),
      [],
      'legacy success arm logs no preflight error',
    );

    // Wire the FAITHFUL success bridge (runs the real workflow Effect through
    // Effect.runPromiseExit — the same Exit the ingress runtime produces).
    board.installEffectRoutes({
      runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
      ...ALL_ROUTE_BUILDERS,
    });

    const workflowRun = await withErrorLog(() =>
      rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' }),
    );
    assertByteIdentical(workflowRun.result, legacy, `preflight ${dialect.name}`);
    assert.deepEqual(
      preflightLogs(workflowRun.errors),
      [],
      'workflow success arm logs no preflight error',
    );
  });
}

test('a core rejection folds to the DISTINCT 500 preflight dialect + the preflight log on BOTH paths, byte- and log-identical', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('preflight probe boom');
  // Force the fold on both paths: the workflow's run capability and the legacy
  // handler both call this same rejecting core.preflightRepo.
  board.core.preflightRepo = () => Promise.reject(boom);

  // Legacy path (effectRoutes null): the `.catch` folds to the 500 dialect + logs.
  const legacyRun = await withErrorLog(() =>
    rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' }),
  );
  const legacy = legacyRun.result;
  assert.equal(legacy.status, 500, 'legacy fold → 500');
  assert.equal(legacy.body, PREFLIGHT_500, 'legacy fold → the DISTINCT preflight dialect');
  assert.notEqual(legacy.body, CONTROL_ASYNC_500, 'never controlAsync {reason:internal}');
  assert.notEqual(legacy.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.deepEqual(
    preflightLogs(legacyRun.errors),
    [[PREFLIGHT_LOG, boom]],
    'legacy logs the preflight line once',
  );

  // Effect path: the workflow folds the same rejection to the same 500 dialect, and
  // its onError logs the identical line — the settler answers via its success branch.
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
  const workflowRun = await withErrorLog(() =>
    rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' }),
  );
  assertByteIdentical(workflowRun.result, legacy, 'preflight fold');
  // Byte-identical wire AND log-identical: same prefix, same error by identity (§6
  // causeSquash preserves the raw rejection across the converted dispatcher).
  assert.deepEqual(
    preflightLogs(workflowRun.errors),
    [[PREFLIGHT_LOG, boom]],
    'workflow logs the identical preflight line once',
  );
});

test('a quiescing ingress REFUSES the preflight POST with 503 shutting-down (mutating-family, NOT a read replay)', async (t) => {
  const board = await startBoard(t);

  // Preflight is a MUTATING-family POST: the workflow effect is never run (the
  // ingress admission refuses), the recorder never starts the native call, so the
  // settler emits the frozen shutdown 503 — the INVERTED policy vs the worktrees
  // READ, which replays to a 200.
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const quiesced = await rawPost(board.port, '/api/repos/preflight', {
    repo: 'octocat/hello-world',
  });
  assert.equal(quiesced.status, 503, 'quiesce → 503 refusal');
  assert.equal(quiesced.body, SHUTDOWN_503, 'quiesce → the frozen shutdown wire');
  assert.equal(quiesced.headers['content-type'], 'application/json');
  assert.equal(quiesced.headers['x-content-type-options'], 'nosniff');
});

test('a workflow defect renders the SAME 500 preflight dialect + the preflight log — never controlAsync internal, never 503', async (t) => {
  const board = await startBoard(t);
  const boom = new Error('unexpected preflight fault');

  // A die (the structurally-unreachable sync-throw arm from section A, or any
  // unexpected fault) surfaces the SAME 500 preflight dialect the fold does — NOT
  // controlAsync's 500 {"reason":"internal"}, NOT CONTROL_DEFECT's {"err":"internal"},
  // NOT the 503 refusal. The recorder never started (runRequest ignores the effect),
  // so the settler throws the defect into its `.catch`, logging the frozen preflight
  // line so a real regression is not swallowed silently.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(boom)),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await withErrorLog(() =>
    rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' }),
  );
  assert.equal(defected.result.status, 500, 'defect → 500, never 503');
  assert.equal(defected.result.body, PREFLIGHT_500, 'defect → the DISTINCT preflight dialect');
  assert.notEqual(defected.result.body, CONTROL_ASYNC_500, 'never controlAsync {reason:internal}');
  assert.notEqual(defected.result.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.notEqual(defected.result.body, SHUTDOWN_503, 'a defect is not a quiesce refusal');
  assert.equal(defected.result.headers['content-type'], 'application/json');
  assert.equal(defected.result.headers['x-content-type-options'], 'nosniff');
  assert.deepEqual(
    preflightLogs(defected.errors),
    [[PREFLIGHT_LOG, boom]],
    'the settler logs the defect with the frozen preflight line',
  );
});

test('transport 400 wall rejects a non-string repo before dispatch (runner/core spy 0 on both paths)', async (t) => {
  // {repo:1} is a TYPE error at the transport wall (repoPreflightBodyError), not
  // DATA from resolveTarget. The wall json(400)+returns BEFORE logExec and
  // BEFORE settleEffectPreflightRoute / the legacy core.preflightRepo call, so
  // neither the recorder nor the dispatcher (hence neither core.preflightRepo
  // nor runControlDetached) may start. B's 400 is repo_org-on-a-URL — that IS
  // DATA from resolveTarget, so the runner DOES run; this pin is the other 400.
  let runnerCalls = 0;
  const wrapping: RunControlDetached = (effect) => {
    runnerCalls += 1;
    return runControlDetached(effect);
  };
  const board = await startBoard(t, { runControlDetached: wrapping });
  let coreCalls = 0;
  const original = board.core.preflightRepo.bind(board.core);
  board.core.preflightRepo = ((body: Parameters<typeof original>[0]) => {
    coreCalls += 1;
    return original(body);
  }) as typeof board.core.preflightRepo;

  const expectWall = (res: RawResponse, label: string): void => {
    assert.equal(res.status, 400, `${label}: status`);
    assert.equal(res.body, '{"ok":false,"reason":"repo must be a string"}', `${label}: body`);
    assert.equal(res.headers['content-type'], 'application/json', `${label}: content-type`);
    assert.equal(res.headers['x-content-type-options'], 'nosniff', `${label}: nosniff`);
  };

  const legacy = await rawPost(board.port, '/api/repos/preflight', { repo: 1 });
  expectWall(legacy, 'legacy wall');
  assert.equal(coreCalls, 0, 'legacy wall never calls core.preflightRepo');
  assert.equal(runnerCalls, 0, 'legacy wall never discharges the runner');

  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });
  const workflow = await rawPost(board.port, '/api/repos/preflight', { repo: 1 });
  expectWall(workflow, 'workflow wall');
  assert.equal(coreCalls, 0, 'workflow wall never calls core.preflightRepo');
  assert.equal(runnerCalls, 0, 'workflow wall never discharges the runner');
});

// ============================ D. CORE DISPATCHER LIVENESS ============================

// resolveTarget needs only these slivers of ctx (mirrors tests/repos.test.ts's
// fakeReposCtx): an empty catalog and no persisted settings. The optional
// runControlDetached is the slice-2 seam — present ⇒ the Effect core runs.
function fakePreflightCtx(runner?: RunControlDetached) {
  const base = {
    q: {
      repoByName: {
        all: () => [] as { repo_name: string; root: string; origin_url: string | null }[],
      },
      getSetting: { get: () => undefined },
    },
    onMutate: () => {
      /* no-op mutation hook for the fake ctx */
    },
  };
  return (runner ? { ...base, runControlDetached: runner } : base) as unknown as Parameters<
    typeof createRepos
  >[0];
}

test('CORE: the preflightRepo dispatcher routes through the injected runner when present, and matches the legacy body when absent', async () => {
  // A network-free, env-independent 400: an empty repo string is rejected by
  // parseRepoInput BEFORE resolveTarget touches the catalog, git, or the default-org
  // env — so the wire is deterministic on BOTH dispatcher legs.
  const EXPECT = { status: 400, body: { ok: false, reason: 'repo must be a non-empty string' } };

  // runner PRESENT ⇒ the dispatcher runs the degenerate Effect core through the
  // injected runControlDetached (Effect.promise over runPreflightRepo).
  let runnerCalls = 0;
  const spy: RunControlDetached = (effect) => {
    runnerCalls += 1;
    return runControlDetached(effect);
  };
  const wired = createRepos(fakePreflightCtx(spy));
  const wiredOut = await wired.preflightRepo({ repo: '' });
  assert.equal(runnerCalls, 1, 'runner invoked exactly once (Effect core is live)');
  assert.deepEqual(wiredOut, EXPECT, 'Effect-core dispatch → the deterministic 400 wire');

  // runner ABSENT ⇒ the dispatcher falls to the verbatim legacy body — the rollback
  // seam — producing the byte-identical wire.
  const legacy = createRepos(fakePreflightCtx());
  const legacyOut = await legacy.preflightRepo({ repo: '' });
  assert.deepEqual(legacyOut, EXPECT, 'runner-absent → the verbatim legacy body');
  assert.deepEqual(legacyOut, wiredOut, 'both dispatcher legs agree to the byte');
});

// =================== E. LIVE INGRESS BRIDGE (JOIN-ON-INTERRUPT) ===================
// The C-section quiesce pin stubs runRequest to Exit.fail(ApplicationQuiescingError)
// WITHOUT running the effect, so recorder.started() === null and the settler 503s.
// That cannot reproduce the ASYNC case the preflight wrapper exists for: a real
// interrupt() landing on an already-admitted preflight whose native
// core.preflightRepo settles only AFTER the interrupt. Here the recorder DID
// capture the Promise, so settleEffectPreflightRoute must JOIN it — never 503.
// A joined native REJECTION is written by onRejected (not the workflow fold) as
// the DISTINCT preflight 500 dialect. Modeled on the control JOIN pin
// (http-workflow-control.test.ts live bridge).

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
// after release(), modelling core.preflightRepo still in flight when the shutdown
// fiber interrupts the request. The preflight dispatch's run thunk reads
// core.preflightRepo at invoke time, so overriding it after createHttp still
// takes effect; the recorder in http.ts captures THIS exact Promise, so `settled`
// flipping before the response resolves witnesses that the settler joined the
// native op rather than 503-ing.
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

test('live bridge: interrupting an in-flight preflight joins the started write (success AND rejection), never 503', async (t) => {
  // (a) Admitted, in flight, then interrupted → JOIN the TRUE preflight DATA
  // result (a stubbed 200 clone) — never 503, never 500.
  const board = await startLiveBoard(t);
  const successWire = {
    status: 200,
    body: { ok: true, mode: 'clone', provider: 'github', transport: 'https' },
  };
  board.core.preflightRepo = () => Promise.resolve(successWire);
  const gate = gateAsyncMethod(board.core, 'preflightRepo');

  const reqP = rawPost(board.port, '/api/repos/preflight', { repo: 'octocat/hello-world' });
  let responded = false;
  void reqP.then(() => {
    responded = true;
  });

  await waitFor(() => gate.invocations() === 1, 'preflightRepo invoked');
  assert.equal(board.supervisor.activeCount, 1, 'the request fiber is in flight');
  assert.equal(gate.settled(), false, 'the native preflight has not settled yet');

  board.supervisor.interrupt();
  assert.equal(board.supervisor.state, 'quiescing', 'interrupt() quiesces admission');

  // The interrupt must NOT collapse to 503: the write already started, so the
  // settler JOINs it. The fiber's Exit resolves (activeCount → 0) but the response
  // stays pending on the still-gated native Promise.
  await waitFor(() => board.supervisor.activeCount === 0, 'the interrupted fiber settled');
  await Bun.sleep(20);
  assert.equal(responded, false, 'response must join the started write, not resolve to 503');
  assert.equal(gate.settled(), false, 'the joined preflight is still gated');

  gate.release();
  const res = await within(reqP, 'joined preflight success');
  assert.equal(gate.settled(), true, 'preflightRepo settled before the response resolved');
  assert.equal(res.status, 200, 'the TRUE preflight DATA result — not 503, not 500');
  assert.equal(res.body, JSON.stringify(successWire.body));
  assert.notEqual(res.body, SHUTDOWN_503, 'not the shutting-down refusal');
  assert.notEqual(res.body, PREFLIGHT_500, 'not the preflight 500 dialect');
  assert.notEqual(res.body, CONTROL_DEFECT_500, 'not CONTROL_DEFECT');
  assert.notEqual(res.body, CONTROL_ASYNC_500, 'not controlAsync {reason:internal}');

  // (b) Rejection-after-start: the native then REJECTS. The fiber is already
  // interrupted, so the workflow fold is not the writer — onRejected must render
  // the DISTINCT preflight 500 dialect (if that literal were {reason:'internal'}
  // or CONTROL_DEFECT, shutdown would emit the wrong 500).
  const rejectBoard = await startLiveBoard(t);
  const boom = new Error('preflight JOIN reject sentinel');
  rejectBoard.core.preflightRepo = () => Promise.reject(boom);
  const rejectGate = gateAsyncMethod(rejectBoard.core, 'preflightRepo');

  const rejectP = rawPost(rejectBoard.port, '/api/repos/preflight', {
    repo: 'octocat/hello-world',
  });
  let rejectResponded = false;
  void rejectP.then(() => {
    rejectResponded = true;
  });

  await waitFor(() => rejectGate.invocations() === 1, 'rejecting preflightRepo invoked');
  rejectBoard.supervisor.interrupt();
  await waitFor(() => rejectBoard.supervisor.activeCount === 0, 'the rejecting fiber settled');
  await Bun.sleep(20);
  assert.equal(rejectResponded, false, 'rejection arm must JOIN, not 503');
  assert.equal(rejectGate.settled(), false, 'the joined rejection is still gated');

  rejectGate.release();
  const rejected = await withErrorLog(() => within(rejectP, 'joined preflight rejection'));
  assert.equal(rejectGate.settled(), true, 'the joined rejection settled before the response');
  assert.equal(rejected.result.status, 500, 'joined rejection → 500, never 503');
  assert.equal(rejected.result.body, PREFLIGHT_500, 'onRejected → the DISTINCT preflight dialect');
  assert.notEqual(rejected.result.body, CONTROL_ASYNC_500, 'never controlAsync {reason:internal}');
  assert.notEqual(rejected.result.body, CONTROL_DEFECT_500, 'never CONTROL_DEFECT {err:internal}');
  assert.notEqual(
    rejected.result.body,
    SHUTDOWN_503,
    'a joined rejection is not a quiesce refusal',
  );
  // Interrupt racing a native rejection may log the prefix twice (workflow fold
  // continuation + onRejected) — same as settleControlAsyncRoute. Bytes stay
  // single; every preflight line must carry the RAW boom by identity.
  const joinLogs = preflightLogs(rejected.errors);
  assert.ok(
    joinLogs.length >= 1 && joinLogs.length <= 2,
    `preflight log count: ${joinLogs.length}`,
  );
  for (const line of joinLogs) {
    assert.deepEqual(
      line,
      [PREFLIGHT_LOG, boom],
      'joined rejection logs the RAW error by identity',
    );
  }
});
