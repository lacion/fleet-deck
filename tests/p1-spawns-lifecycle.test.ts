import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

interface ControlResult {
  status: number;
  body: {
    ok?: boolean;
    reason?: string;
    spawn_id?: string;
    session_id?: string;
  };
}

interface StatusRow {
  status: string;
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

function pause(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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

test('P1 spawn lifecycle aborts and joins detached clone provisioning before DB close', async (t) => {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p1-clone-'));
  const shimDir = path.join(scratch, 'bin');
  const reposDir = path.join(scratch, 'repos');
  const started = path.join(scratch, 'clone-started');
  const aborted = path.join(scratch, 'clone-aborted');
  mkdirSync(shimDir, { recursive: true });
  const gitShim = path.join(shimDir, 'git');
  writeFileSync(
    gitShim,
    [
      '#!/bin/sh',
      'case "$1" in',
      '  check-ref-format|ls-remote) exit 0 ;;',
      '  clone)',
      // Arm the abort trap BEFORE announcing readiness: the test gates `close()`
      // on the `started` file, so once it appears the shim must already catch a
      // TERM. Writing `started` first leaves a window where a SIGTERM delivered
      // under load (before `trap` runs) kills the shim on the default
      // disposition and `aborted` is never written — the flake this test hit in
      // the full suite.
      '    trap \'printf aborted > "$FD_P1_CLONE_ABORTED"; exit 143\' TERM INT',
      '    printf started > "$FD_P1_CLONE_STARTED"',
      '    while :; do sleep 1; done',
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
    FD_P1_CLONE_STARTED: started,
    FD_P1_CLONE_ABORTED: aborted,
  });
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  const db = openDb(':memory:');
  let dbOpen = true;
  const core = createCore(db, {
    port: 4711,
    home: scratch,
    tmuxAdapter: makeAdapter(),
  });
  t.after(async () => {
    await core.lifecycle.close();
    if (dbOpen) db.close();
  });
  const spawned = (await core.spawn({
    repo: 'https://example.com/fleetdeck-p1.git',
    branch: 'main',
    branch_mode: 'in-place',
  })) as ControlResult;
  assert.equal(spawned.status, 202, spawned.body.reason);
  assert.ok(spawned.body.spawn_id);
  await waitUntil(() => existsSync(started), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'blocking clone to start',
  });

  const closeA = core.lifecycle.close();
  const closeB = core.lifecycle.close();
  assert.strictEqual(closeB, closeA, 'double close shares the same join promise');
  await Promise.race([
    closeA,
    pause(2_000).then(() => {
      throw new Error('spawn lifecycle did not abort/join clone provisioning');
    }),
  ]);
  await waitUntil(() => existsSync(aborted), {
    timeoutMs: 2_000,
    intervalMs: 10,
    label: 'clone shim to observe abort',
  });
  const row = db
    .prepare<StatusRow>('SELECT status FROM spawns WHERE spawn_id = ?')
    .get(spawned.body.spawn_id);
  assert.equal(row?.status, 'gone', 'owned compensation settled the provisional row');

  const refused = (await core.spawn({ cwd: scratch })) as ControlResult;
  assert.equal(refused.status, 503, 'post-quiesce launch admission is closed');
  db.close();
  dbOpen = false;
});

test('P1 spawn lifecycle joins in-flight liveness and suppresses post-quiesce reconciliation', async (t) => {
  const db = openDb(':memory:');
  const windows = deferred<[]>();
  let blockWindows = false;
  let listCalls = 0;
  const core = createCore(db, {
    port: 4711,
    home: '/p1-spawn-liveness',
    tmuxAdapter: makeAdapter({
      listScopedWindows: () => {
        listCalls++;
        return blockWindows ? windows.promise : Promise.resolve([]);
      },
    }),
  });
  t.after(async () => {
    await core.lifecycle.close();
  });
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions
      (session_id, callsign, col, started_at, last_seen, source)
     VALUES ('p1-live', 'otter-p1', 'idle', ?, ?, 'spawned')`,
  ).run(now, now);
  db.prepare(
    `INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, requested_at, status)
     VALUES ('p1-live-spawn', 'p1-live', 'otter-p1', 'fleetdeck-4711', 'fd4711-otter-p1', ?, 'live')`,
  ).run(now);

  blockWindows = true;
  const liveness = core.spawnLivenessTick();
  const callsAtBlock = listCalls;
  const close = core.lifecycle.close();
  let closed = false;
  void close.then(() => {
    closed = true;
  });
  await Promise.resolve();
  assert.equal(closed, false, 'close waits for the accepted liveness probe');

  await core.reconcileSpawns();
  assert.equal(listCalls, callsAtBlock, 'post-quiesce reconciliation is a no-op');
  windows.resolve([]);
  await Promise.all([liveness, close]);
  db.close();
  await pause(20);
  assert.equal(listCalls, callsAtBlock, 'no maintenance callback runs after DB close');
});

test('P1 spawn lifecycle cancels store timers and phase-guards late launchOverride errors', async (t) => {
  setEnv(t, {
    FLEETDECK_RC_HARVEST_MS: '60',
    FLEETDECK_ADOPT_DELAY_MS: '60',
    FLEETDECK_CLEAR_SETTLE_MS: '60',
    FLEETDECK_NUDGE_MS: '60',
  });
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-p1-timers-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  let captureCalls = 0;
  const overrideCallbacks: { error?: (err: unknown) => void } = {};
  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 4711,
    home: cwd,
    tmuxAdapter: makeAdapter({
      spawnOverrideCmd: () => '/fake-spawn-override',
      capturePane: () => {
        captureCalls++;
        return Promise.resolve('https://claude.ai/code/session_p1');
      },
      launchOverride: (_command, _spec, onError) => {
        if (onError) overrideCallbacks.error = onError;
      },
    }),
  });
  t.after(async () => {
    await core.lifecycle.close();
  });
  let mutations = 0;
  core.onMutate = () => {
    mutations++;
  };

  const spawned = (await core.spawn({
    cwd,
    prompt: 'timer ownership',
    remote_control: true,
  })) as ControlResult;
  assert.equal(spawned.status, 200, spawned.body.reason);
  assert.ok(spawned.body.session_id);
  core.hookSessionStart({ session_id: spawned.body.session_id, cwd, source: 'startup' });

  const armedSid = 'p1-armed-adopt';
  core.hookSessionStart({ session_id: armedSid, cwd, source: 'startup' });
  const armed = (await core.adoptSession(
    armedSid,
    { dangerously_skip_permissions: false },
    { deferred: false },
  )) as ControlResult;
  assert.equal(armed.status, 200);
  core.hookSessionEnd({ session_id: armedSid, cwd, reason: 'logout' });

  const predecessor = 'p1-clear-predecessor';
  const heir = 'p1-clear-heir';
  core.hookSessionStart({ session_id: predecessor, cwd, source: 'startup' });
  core.hookSessionStart({ session_id: heir, cwd, source: 'clear' });
  core.hookSessionEnd({ session_id: predecessor, cwd, reason: 'clear' });

  const launchError = overrideCallbacks.error;
  assert.ok(launchError, 'override error callback was captured');
  await core.lifecycle.close();
  const predecessorRow = db
    .prepare<{ succeeded_by: string | null }>(
      'SELECT succeeded_by FROM sessions WHERE session_id = ?',
    )
    .get(predecessor);
  assert.equal(predecessorRow?.succeeded_by, null, 'clear settlement timer was cancelled');
  const mutationsAtClose = mutations;
  db.close();

  // launchOverride has no completion handle. Its callback can arrive after
  // close, so the owner must refuse the compensation callback before it reads
  // or mutates SQLite.
  launchError(new Error('late override failure'));
  await pause(100);
  assert.equal(captureCalls, 0, 'nudge and remote-harvest timers stayed stopped');
  assert.equal(mutations, mutationsAtClose, 'no delayed store callback mutated after close');
});
