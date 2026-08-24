// http-workflow-health-state.test.ts — the focused fixture for the P6.4 pilot
// route group (GET /health, GET /state). It pins the CONVENTION established in
// src/daemon/app/http-workflows/health-state.ts three ways:
//
//   A. ISOLATION — the workflow Effects and the Exit → Response mapper in pure
//      isolation, with capability fakes, asserting the exact payload shapes, the
//      frozen key order, lazy thunk resolution, and every mapper case.
//   B. REAL DAEMON WIRE — a real daemon (program.ts wires installEffectRoutes to
//      the live ingress bridge) answers /health and /state with the frozen bytes:
//      status, the pinnable headers, and the body fields incl. the /state lan block.
//   C. IN-PROCESS EQUIVALENCE — on ONE idle in-memory core, toggling the bridge on
//      the SAME createHttp handle proves the workflow path is byte-identical to the
//      legacy handler, that a quiescing ingress falls back to the legacy handler
//      with identical bytes, and that a workflow defect reproduces the legacy 500.
//
// The wire contract is byte-for-byte (docs/v1/evidence/effect/p6-http-matrix.md):
// /health and /state are always-200 snapshot reads, answered tokenless on the
// loopback bind via the authorized() loopback waiver.

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
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
  type HealthCapabilities,
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
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';

import { REPO_ROOT, startDaemon } from '../helpers/daemon.ts';
import test, { type TestContext } from '../helpers/harness-test.ts';

// The frozen GET /health key order — the board reads `auth`/`spawn` off it, and a
// reordered body is a wire break the freeze suite would (separately) catch.
const HEALTH_KEY_ORDER = [
  'ok',
  'fleet',
  'pid',
  'version',
  'managed',
  'spawn',
  'auth',
  'startup',
] as const;

// P9.5 Slice 1 — GET /api/settings. The envelope is { ok, settings } and the
// settings body is core.resolveSettings() verbatim: a FROZEN sync leaf shared by
// GET /api/settings, the POST /api/settings response body, and the /state
// broadcast. Its object-literal key order is the wire contract, and its `gateway`
// is MASKED (a token_set boolean; NEVER the raw token) — a credential-safety
// invariant the transport must pass through untouched. Values are
// environment-dependent (real home/config paths), so the pins below assert this
// STRUCTURE, not the paths, plus legacy↔workflow byte-identity for the exact bytes.
const SETTINGS_ENVELOPE_KEY_ORDER = ['ok', 'settings'] as const;
const SETTINGS_KEY_ORDER = [
  'repos_dir',
  'repo_transport',
  'repo_default_org',
  'browse_root',
  'fav_dirs',
  'repo_setup',
  'hold_ms',
  'gateway',
] as const;
const GATEWAY_KEY_ORDER = [
  'base_url',
  'auth_style',
  'token_set',
  'model_discovery',
  'default',
  'ready',
] as const;

// Assert the frozen GET /api/settings envelope + the masked-gateway invariant on a
// parsed body, independent of the environment-specific path VALUES.
function assertSettingsEnvelope(body: Record<string, unknown>, label: string): void {
  assert.deepEqual(Object.keys(body), [...SETTINGS_ENVELOPE_KEY_ORDER], `${label}: envelope keys`);
  assert.equal(body['ok'], true, `${label}: ok:true`);
  const settings = body['settings'] as Record<string, unknown>;
  assert.deepEqual(Object.keys(settings), [...SETTINGS_KEY_ORDER], `${label}: settings keys`);
  const gateway = settings['gateway'] as Record<string, unknown>;
  assert.deepEqual(Object.keys(gateway), [...GATEWAY_KEY_ORDER], `${label}: gateway keys`);
  // The masking invariant: token presence is a boolean, and the raw token never
  // reaches the wire under any key.
  assert.equal(typeof gateway['token_set'], 'boolean', `${label}: gateway.token_set is boolean`);
  assert.equal('token' in gateway, false, `${label}: gateway must not leak a raw token`);
}

// Port growth: HttpEffectRoutes requires every converted group's builders. These
// pilot tests only exercise /health and /state, but installEffectRoutes requires
// the whole port, so every group's real builders are wired unchanged to satisfy
// the type (the other groups are never reached here — each has its own focused
// suite: paste, settings-command-mail-cleanup, control).
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
};

// ============================ A. ISOLATION ============================

test('healthWorkflow assembles the frozen /health payload in exact key order', () => {
  const caps: HealthCapabilities = {
    fleet: () => 3,
    pid: 4242,
    version: '9.9.9-fake',
    managed: true,
    spawn: () => ({ available: true }),
    auth: { term_token: false },
    startup: () => 'settled',
  };
  const payload = Effect.runSync(healthWorkflow(caps));
  assert.deepEqual(payload, {
    ok: true,
    fleet: 3,
    pid: 4242,
    version: '9.9.9-fake',
    managed: true,
    spawn: { available: true },
    auth: { term_token: false },
    startup: 'settled',
  });
  assert.deepEqual(Object.keys(payload), [...HEALTH_KEY_ORDER]);
});

test('healthWorkflow resolves capability thunks lazily, inside the Effect', () => {
  let fleetCalls = 0;
  let spawnCalls = 0;
  let startupCalls = 0;
  const caps: HealthCapabilities = {
    fleet: () => {
      fleetCalls += 1;
      return 1;
    },
    pid: 7,
    version: 'x',
    managed: false,
    spawn: () => {
      spawnCalls += 1;
      return null;
    },
    auth: { term_token: true },
    startup: () => {
      startupCalls += 1;
      return null;
    },
  };
  const effect = healthWorkflow(caps);
  // Building the Effect must touch no capability — reads happen when it runs.
  assert.equal(fleetCalls, 0);
  assert.equal(spawnCalls, 0);
  assert.equal(startupCalls, 0);
  Effect.runSync(effect);
  assert.equal(fleetCalls, 1);
  assert.equal(spawnCalls, 1);
  assert.equal(startupCalls, 1);
});

test('stateWorkflow returns the snapshotWithLan result verbatim', () => {
  const marker = { session: 'a', lan: { enabled: false, urls: [] }, legacy_upgrade: null };
  let calls = 0;
  const out = Effect.runSync(
    stateWorkflow({
      snapshotWithLan: () => {
        calls += 1;
        return marker;
      },
    }),
  );
  // Same reference, resolved exactly once inside the Effect — no copy, no reshape.
  assert.equal(out, marker);
  assert.equal(calls, 1);
});

test('settingsSnapshotWorkflow wraps resolve() as { ok:true, settings } in frozen key order', () => {
  // The workflow only wraps: whatever resolve() returns is relayed verbatim under
  // `settings` (so the masked gateway a real leaf produces passes through untouched),
  // and the envelope key order is exactly [ok, settings].
  const marker = { repos_dir: { value: '/x' }, gateway: { token_set: false } };
  let calls = 0;
  const payload = Effect.runSync(
    settingsSnapshotWorkflow({
      resolve: () => {
        calls += 1;
        return marker;
      },
    }),
  );
  assert.deepEqual(payload, { ok: true, settings: marker });
  assert.deepEqual(Object.keys(payload), [...SETTINGS_ENVELOPE_KEY_ORDER]);
  // Same reference under `settings` — no copy, no reshape.
  assert.equal((payload as { settings: unknown }).settings, marker);
  assert.equal(calls, 1);
});

test('settingsSnapshotWorkflow resolves its capability thunk lazily, inside the Effect', () => {
  let calls = 0;
  const effect = settingsSnapshotWorkflow({
    resolve: () => {
      calls += 1;
      return {};
    },
  });
  // Building the Effect must touch no capability — the read happens when it runs.
  assert.equal(calls, 0);
  Effect.runSync(effect);
  assert.equal(calls, 1);
});

test('mapEffectRouteExit classifies success, quiesce, and defect exits', () => {
  // success → the workflow value, verbatim.
  assert.deepEqual(mapEffectRouteExit(Exit.succeed(42)), { kind: 'success', value: 42 });

  // quiesce → an ApplicationQuiescingError fail, detected structurally by _tag.
  const quiescing = new ApplicationQuiescingError({
    operation: 'GET /health',
    message: 'quiescing',
  });
  assert.deepEqual(mapEffectRouteExit(Exit.fail(quiescing)), { kind: 'quiesce' });

  // defect → a die carries its defect through untouched.
  const boom = new Error('boom');
  const died = mapEffectRouteExit(Exit.die(boom));
  assert.equal(died.kind, 'defect');
  assert.equal(died.kind === 'defect' ? died.defect : null, boom);

  // defect → an UNEXPECTED (non-quiesce) fail on an E=never route is treated as a
  // defect too, so the route emits the same 500 the legacy catch would.
  const other = new Error('nope');
  const failed = mapEffectRouteExit(Exit.fail(other));
  assert.equal(failed.kind, 'defect');
  assert.equal(failed.kind === 'defect' ? failed.defect : null, other);
});

// ============================ B. REAL DAEMON WIRE ============================

interface RawResponse {
  readonly status: number | undefined;
  readonly headers: Record<string, string | string[] | undefined>;
  readonly body: string;
}

// One request over the real socket, capturing status + headers + body so the
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
      let body = '';
      res.on('data', (d: Buffer) => {
        body += d.toString();
      });
      res.on('end', () => {
        resolve({ status: res.statusCode, headers: res.headers, body });
      });
    });
    req.setTimeout(5000, () => req.destroy(new Error('raw request timed out')));
    req.on('error', reject);
    req.end();
  });
}

test('the wired daemon answers /health and /state with the frozen bytes', async () => {
  const daemon = await startDaemon();
  try {
    // /health — tokenless on the loopback bind (authorized() waiver).
    const health = await fetch(`${daemon.baseUrl}/health`);
    assert.equal(health.status, 200);
    assert.equal(health.headers.get('content-type'), 'application/json');
    assert.equal(health.headers.get('x-content-type-options'), 'nosniff');
    const healthText = await health.text();
    const healthCl = health.headers.get('content-length');
    if (healthCl !== null) assert.equal(healthCl, String(Buffer.byteLength(healthText)));
    const healthBody = JSON.parse(healthText) as Record<string, unknown>;
    assert.deepEqual(Object.keys(healthBody), [...HEALTH_KEY_ORDER]);
    assert.equal(healthBody['ok'], true);
    assert.equal(healthBody['pid'], daemon.proc.pid);
    const pkg = JSON.parse(readFileSync(path.join(REPO_ROOT, 'package.json'), 'utf8')) as {
      version: string;
    };
    assert.equal(
      healthBody['version'],
      process.env['FLEETDECK_VERSION_OVERRIDE']?.trim() || pkg.version,
    );

    // /state — the token-gated snapshot; the lan block stays exactly as today.
    const state = await fetch(`${daemon.baseUrl}/state`, {
      headers: { authorization: `Bearer ${daemon.token}` },
    });
    assert.equal(state.status, 200);
    assert.equal(state.headers.get('content-type'), 'application/json');
    assert.equal(state.headers.get('x-content-type-options'), 'nosniff');
    const stateText = await state.text();
    const stateCl = state.headers.get('content-length');
    if (stateCl !== null) assert.equal(stateCl, String(Buffer.byteLength(stateText)));
    const stateBody = JSON.parse(stateText) as Record<string, unknown>;
    // No LAN configured under startDaemon ⇒ the panel collapses to local-only.
    assert.deepEqual(stateBody['lan'], { enabled: false, urls: [] });
    assert.ok('legacy_upgrade' in stateBody, 'the snapshot banner must ride /state');
    const stateKeys = Object.keys(stateBody);
    assert.equal(stateKeys[stateKeys.length - 2], 'lan');
    assert.equal(stateKeys[stateKeys.length - 1], 'legacy_upgrade');

    // /api/settings — the P9.5 Slice 1 always-200 snapshot read, served through the
    // live ingress bridge. Structural pin (values are env-specific): the { ok,
    // settings } envelope, the frozen settings key order, and the masked gateway.
    const settings = await fetch(`${daemon.baseUrl}/api/settings`, {
      headers: { authorization: `Bearer ${daemon.token}` },
    });
    assert.equal(settings.status, 200);
    assert.equal(settings.headers.get('content-type'), 'application/json');
    assert.equal(settings.headers.get('x-content-type-options'), 'nosniff');
    const settingsText = await settings.text();
    const settingsCl = settings.headers.get('content-length');
    if (settingsCl !== null) assert.equal(settingsCl, String(Buffer.byteLength(settingsText)));
    assertSettingsEnvelope(JSON.parse(settingsText) as Record<string, unknown>, '/api/settings');
  } finally {
    await daemon.stop();
  }
  assert.equal(daemon.proc.exitCode, 0, `stderr: ${daemon.stderr}`);
});

// ============================ C. IN-PROCESS EQUIVALENCE ============================

type BoardHandle = ReturnType<typeof createHttp> & { port: number };

// The network-refresh in-process harness: an idle :memory: core behind createHttp,
// bound on a real loopback port (the Host wall pins Host's port to the configured
// port, so bind first then hand createHttp the real port). effectRoutes starts
// null — the legacy path — and the test installs the bridge when it wants.
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

// /health carries no clock field, so its bytes are strictly identical across two
// captures — the workflow path and the legacy path must agree to the byte.
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

// /state's snapshot embeds a LIVE uptime (up_ms/uptime_ms) that advances between
// the two sequential captures regardless of which code path served them — a
// clock, not a migration difference. Neutralise ONLY those two fields (preserving
// key order via the in-place spread) and require every other byte to match. The
// header trio must still agree exactly on content-type + nosniff.
function normalizeStateBody(body: string): unknown {
  const parsed = JSON.parse(body) as Record<string, unknown>;
  return { ...parsed, up_ms: 0, uptime_ms: 0 };
}

function assertStateEquivalent(actual: RawResponse, expected: RawResponse, label: string): void {
  assert.equal(actual.status, expected.status, `${label}: status`);
  assert.deepEqual(
    normalizeStateBody(actual.body),
    normalizeStateBody(expected.body),
    `${label}: body (modulo the live uptime clock)`,
  );
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
}

test('workflow dispatch is byte-identical to the legacy handler for /health and /state', async (t) => {
  const board = await startBoard(t);

  // effectRoutes null ⇒ the legacy synchronous handlers answer. Capture them.
  const legacyHealth = await rawFull(board.port, { path: '/health' });
  const legacyState = await rawFull(board.port, { path: '/state' });
  const legacySettings = await rawFull(board.port, { path: '/api/settings' });
  assert.equal(legacyHealth.status, 200);
  assert.equal(legacyState.status, 200);
  assert.equal(legacyHealth.headers['content-type'], 'application/json');
  assert.equal(legacyHealth.headers['x-content-type-options'], 'nosniff');
  assert.deepEqual(Object.keys(JSON.parse(legacyHealth.body) as Record<string, unknown>), [
    ...HEALTH_KEY_ORDER,
  ]);
  // Pin the LEGACY /api/settings dialect explicitly BEFORE the bridge exists: an
  // always-200 { ok, settings } read with the masked gateway (the frozen bytes the
  // workflow path must then reproduce to the byte).
  assert.equal(legacySettings.status, 200);
  assert.equal(legacySettings.headers['content-type'], 'application/json');
  assert.equal(legacySettings.headers['x-content-type-options'], 'nosniff');
  assertSettingsEnvelope(
    JSON.parse(legacySettings.body) as Record<string, unknown>,
    '/api/settings legacy',
  );

  // Wire the bridge with a FAITHFUL success bridge (runs the real workflow Effect
  // through Effect.runPromiseExit — the same Exit the ingress runtime produces).
  board.installEffectRoutes({
    runRequest: (_operation, effect) => Effect.runPromiseExit(effect),
    ...ALL_ROUTE_BUILDERS,
  });

  const workflowHealth = await rawFull(board.port, { path: '/health' });
  const workflowState = await rawFull(board.port, { path: '/state' });
  const workflowSettings = await rawFull(board.port, { path: '/api/settings' });
  assertByteIdentical(workflowHealth, legacyHealth, '/health');
  assertStateEquivalent(workflowState, legacyState, '/state');
  // /api/settings carries no clock field, so the workflow bytes are strictly
  // identical to the legacy read's — the whole point of the transport-only slice.
  assertByteIdentical(workflowSettings, legacySettings, '/api/settings');
});

test('a quiescing ingress falls back to the legacy handler with identical bytes', async (t) => {
  const board = await startBoard(t);

  const legacyHealth = await rawFull(board.port, { path: '/health' });
  const legacyState = await rawFull(board.port, { path: '/state' });
  const legacySettings = await rawFull(board.port, { path: '/api/settings' });

  // The ingress runtime, while quiescing, resolves runRequest to a failed Exit
  // carrying ApplicationQuiescingError WITHOUT running the workflow. The mapper
  // reports 'quiesce' and the transport falls back to the legacy handler.
  board.installEffectRoutes({
    runRequest: (operation, _effect) =>
      Promise.resolve(
        Exit.fail(new ApplicationQuiescingError({ operation, message: 'daemon is quiescing' })),
      ),
    ...ALL_ROUTE_BUILDERS,
  });

  const quiesceHealth = await rawFull(board.port, { path: '/health' });
  const quiesceState = await rawFull(board.port, { path: '/state' });
  const quiesceSettings = await rawFull(board.port, { path: '/api/settings' });
  assertByteIdentical(quiesceHealth, legacyHealth, '/health during quiesce');
  assertStateEquivalent(quiesceState, legacyState, '/state during quiesce');
  // /api/settings is a snapshot READ: a quiescing ingress replays the legacy 200
  // read (settleEffectSnapshotRoute), never a 503 — a read answers as before shutdown.
  assertByteIdentical(quiesceSettings, legacySettings, '/api/settings during quiesce');
});

test('an interrupts-only Exit maps to quiesce and falls back to the legacy 200', async (t) => {
  // ISOLATION: a Cause whose reasons are ALL interrupts — the shutdown fiber
  // cancelling this in-flight request — classifies as quiesce, not a defect, so
  // the always-200 snapshot contract still holds. Built via the rc.110 API:
  // Cause.interrupt() makes an interrupt-only Cause and Exit.failCause wraps it as
  // a failed Exit, the same shape the ingress runtime yields on a mid-flight kill.
  const interrupted = Exit.failCause(Cause.interrupt(1));
  assert.deepEqual(mapEffectRouteExit(interrupted), { kind: 'quiesce' });

  // TRANSPORT: a bridge that resolves runRequest to that same interrupts-only Exit
  // must fall back to the legacy synchronous handler with identical bytes, exactly
  // like the explicit ApplicationQuiescingError refusal above.
  const board = await startBoard(t);
  const legacyHealth = await rawFull(board.port, { path: '/health' });
  const legacyState = await rawFull(board.port, { path: '/state' });
  const legacySettings = await rawFull(board.port, { path: '/api/settings' });

  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.failCause(Cause.interrupt(1))),
    ...ALL_ROUTE_BUILDERS,
  });

  const interruptedHealth = await rawFull(board.port, { path: '/health' });
  const interruptedState = await rawFull(board.port, { path: '/state' });
  const interruptedSettings = await rawFull(board.port, { path: '/api/settings' });
  assertByteIdentical(interruptedHealth, legacyHealth, '/health during interrupt');
  assertStateEquivalent(interruptedState, legacyState, '/state during interrupt');
  assertByteIdentical(interruptedSettings, legacySettings, '/api/settings during interrupt');
});

test('a workflow defect reproduces the legacy 500 {} exactly', async (t) => {
  const board = await startBoard(t);

  // A die (an unexpected fault) must surface as the byte-identical 500 the legacy
  // outer catch already emits for a non-hook route — never the fail-open 200.
  board.installEffectRoutes({
    runRequest: (_operation, _effect) => Promise.resolve(Exit.die(new Error('boom'))),
    ...ALL_ROUTE_BUILDERS,
  });

  const defected = await rawFull(board.port, { path: '/health' });
  assert.equal(defected.status, 500);
  assert.equal(defected.body, '{}');
  assert.equal(defected.headers['content-type'], 'application/json');
  assert.equal(defected.headers['x-content-type-options'], 'nosniff');

  // /api/settings rides the SAME snapshot settler, so a defect there is the same
  // byte-identical 500 {} the outer catch emits (UNREACHABLE in practice —
  // resolveSettings is a synchronous, non-throwing frozen leaf — but the arm is
  // wired, so pin it like /health.).
  const defectedSettings = await rawFull(board.port, { path: '/api/settings' });
  assert.equal(defectedSettings.status, 500);
  assert.equal(defectedSettings.body, '{}');
  assert.equal(defectedSettings.headers['content-type'], 'application/json');
  assert.equal(defectedSettings.headers['x-content-type-options'], 'nosniff');
});
