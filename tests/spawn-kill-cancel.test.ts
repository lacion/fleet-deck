import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { makeRemoteRepo } from './helpers/gitrepo.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

// ---------------------------------------------------------------------------
// Characterization (P9.1 Slice 3): pin BOTH arms of spawnKill's
// provisioning-cancel race — the exact gap the design doc (§3 Slice 3) names.
//
// When Kill hits a `provisioning` row it aborts the in-flight clone, then races
// the operation's `done` promise against a HARD-CODED 5s bound:
//
//   * op.done wins       → follow-through: 200 {ok:true, status:'cancelled'}
//   * the 5s bound wins   → 202 {ok:true,  status:'cancelling'}
//
// The 5s bound has NO env seam (spawns.ts hard-codes 5_000, unref'd), and a
// real clone cannot outlive an abort — the process driver escalates SIGTERM to
// a process-group SIGKILL after PROCESS_DRIVER_KILL_GRACE_MS (1s), so a
// signal-ignoring shim dies at ~1s, far short of 5s. The ONLY async in the
// abort→resolve path that survives an abort (it never re-checks the signal) and
// is injectable is `tmuxAdapter.ensureSession` inside launchPane, reached after
// a successful clone+materialize. Case 2 therefore drives a REAL local clone to
// launchPane and gates ensureSession open forever ("a gated op.done" — the
// design's own words), so provisioningDone stays pending and the 5s bound wins
// deterministically.
//
// Both cases wire the ingress `runControlDetached` runner (as dismiss.test.ts
// does) so the SAME assertions exercise the legacy async body before the
// conversion and the Effect core after it — byte-identical across the flip.
// ---------------------------------------------------------------------------

type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

interface ControlResult {
  status: number;
  body: {
    ok?: boolean;
    reason?: string;
    spawn_id?: string;
    status?: string;
  };
}

interface StatusRow {
  status: string;
}

interface CountRow {
  n: number;
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

function setEnv(t: TestContext, values: Record<string, string>): void {
  const previous = new Map<string, string | undefined>();
  for (const [name, value] of Object.entries(values)) {
    previous.set(name, process.env[name]);
    process.env[name] = value;
  }
  t.after(() => {
    for (const [name, value] of previous) {
      if (value === undefined) Reflect.deleteProperty(process.env, name);
      else process.env[name] = value;
    }
  });
}

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

function countEvents(db: ReturnType<typeof openDb>, hookEvent: string): number {
  const row = db
    .prepare<CountRow>('SELECT COUNT(*) AS n FROM events WHERE hook_event = ?')
    .get(hookEvent);
  return row?.n ?? 0;
}

function spawnStatus(db: ReturnType<typeof openDb>, spawn_id: string): string | undefined {
  return db.prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id = ?').get(spawn_id)
    ?.status;
}

// Case 1 — op.done wins the race → 200 {ok:true, status:'cancelled'}.
//
// A blocking clone shim traps TERM and exits fast, so the abort settles the
// operation (compensation marks the row 'gone' and logs SpawnCancelled) well
// before the 5s bound. The race resolves 'done', the post-race status re-read
// sees 'gone', and Kill reports the cancelled follow-through.
test('spawnKill cancel: op.done wins the race → 200 cancelled', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-kill-done-'));
  const shimDir = path.join(scratch, 'bin');
  const reposDir = path.join(scratch, 'repos');
  const started = path.join(scratch, 'clone-started');
  mkdirSync(shimDir, { recursive: true });
  const gitShim = path.join(shimDir, 'git');
  writeFileSync(
    gitShim,
    [
      '#!/bin/sh',
      'case "$1" in',
      '  check-ref-format|ls-remote) exit 0 ;;',
      '  clone)',
      // Arm the trap BEFORE announcing readiness so a TERM under load cannot
      // land on the default disposition (the p1 clone-abort idiom).
      "    trap 'exit 143' TERM INT",
      '    printf started > "$FD_KILL_CLONE_STARTED"',
      '    while :; do sleep 1 & wait; done',
      '    ;;',
      'esac',
      'exit 0',
      '',
    ].join('\n'),
  );
  chmodSync(gitShim, 0o755);
  setEnv(t, {
    PATH: `${shimDir}:${process.env['PATH'] ?? ''}`,
    FLEETDECK_REPOS_DIR: reposDir,
    FLEETDECK_CLONE_TIMEOUT_MS: '30000',
    FD_KILL_CLONE_STARTED: started,
  });
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 4711,
    home: scratch,
    tmuxAdapter: makeAdapter(),
    // Wire the ingress runner so the Effect core (post-conversion) is the path
    // under test; harmless no-op for the legacy async body (pre-conversion).
    runControlDetached,
  });
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  const spawned = (await core.spawn({
    repo: 'https://example.com/fleetdeck-kill-done.git',
    branch: 'main',
    branch_mode: 'in-place',
  })) as ControlResult;
  assert.equal(spawned.status, 202, spawned.body.reason);
  const spawn_id = spawned.body.spawn_id ?? '';
  assert.ok(spawn_id, 'a provisioning spawn id is returned');
  await waitUntil(() => existsSync(started), {
    timeoutMs: 4_000,
    intervalMs: 10,
    label: 'blocking clone to start',
  });

  const killed = (await core.spawnKill(spawn_id, true)) as ControlResult;
  assert.deepEqual(
    killed,
    { status: 200, body: { ok: true, spawn_id, status: 'cancelled' } },
    'op.done resolution short-circuits the race → cancelled follow-through',
  );
  assert.equal(spawnStatus(db, spawn_id), 'gone', 'compensation settled the provisional row');
  assert.equal(countEvents(db, 'SpawnCancelled'), 1, 'exactly one SpawnCancelled tombstone');
});

// Case 2 — the 5s bound wins the race → 202 {ok:true, status:'cancelling'}.
//
// A REAL local clone succeeds and provisioning reaches launchPane, which hangs
// on a tmuxAdapter.ensureSession gate that never resolves — so provisioningDone
// stays pending. Kill's abort cannot interrupt the hung await, the 5s bound
// fires first, and Kill reports the cancelling timeout-arm. The gate is released
// in teardown so the owned provisioning chain unwinds and close() can join it.
test('spawnKill cancel: the 5s bound wins the race → 202 cancelling', async (t) => {
  const remote = makeRemoteRepo({});
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-kill-bound-'));
  const reposDir = path.join(scratch, 'repos');
  mkdirSync(reposDir, { recursive: true });
  setEnv(t, {
    FLEETDECK_REPOS_DIR: reposDir,
    FLEETDECK_CLONE_TIMEOUT_MS: '30000',
  });

  // launchPane's first injectable await; gated open forever so op.done stays
  // pending. `entered` proves provisioning has parked here before Kill fires.
  const gate = deferred<string>();
  let entered = false;
  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 4711,
    home: scratch,
    tmuxAdapter: makeAdapter({
      ensureSession: () => {
        entered = true;
        return gate.promise;
      },
    }),
    runControlDetached,
  });
  // Teardown runs in registration (FIFO) order: release the launchPane gate and
  // join the owned provisioning chain (close) BEFORE removing the clone tree, so
  // compensation unwinds against a live worktree rather than a deleted one.
  t.after(async () => {
    // Release the launchPane gate so the aborted chain unwinds (post-ensure
    // abort check throws → compensate → resolveProvisioning) and close() joins.
    gate.resolve('fleetdeck-4711');
    await core.lifecycle.close();
    db.close();
  });
  t.after(() => {
    remote.cleanup();
    rmSync(scratch, { recursive: true, force: true });
  });

  const spawned = (await core.spawn({
    repo: remote.origin,
    branch: 'main',
    branch_mode: 'in-place',
  })) as ControlResult;
  assert.equal(spawned.status, 202, spawned.body.reason);
  const spawn_id = spawned.body.spawn_id ?? '';
  assert.ok(spawn_id, 'a provisioning spawn id is returned');
  await waitUntil(() => entered, {
    timeoutMs: 12_000,
    intervalMs: 10,
    label: 'provisioning to reach the launchPane gate',
  });

  const killed = (await core.spawnKill(spawn_id, true)) as ControlResult;
  assert.deepEqual(
    killed,
    { status: 202, body: { ok: true, spawn_id, status: 'cancelling' } },
    'the 5s bound wins → cancelling timeout-arm bytes',
  );
});
