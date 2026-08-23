import assert from 'node:assert/strict';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import { runControlDetached } from '../src/daemon/platform/bun/ingress-supervisor-live.ts';
import test from './helpers/harness-test.ts';
import { waitUntil } from './helpers/wait.ts';

// ---------------------------------------------------------------------------
// Characterization (P9.1 Slice 6): pin spawn()'s PRE-ABORT QUIESCE guard — the
// one clone-path exit the existing suites never reach (spawns.ts:2084).
//
// A spawn admitted while maintenance was OPEN can still find shutdown underway
// by the time its access/materialization awaits return: between admission and
// the point where the detached provisioning chain is forked, spawn() re-reads
// `spawnMaintenance.isOpen()`. If it is now closed it must NOT launch a fresh
// clone — it compensates the already-durable provisional row (tombstone, status
// -> 'gone', a single SpawnCancelled event) and returns a distinct 503:
//
//     { ok:false, reason:'daemon is shutting down; spawn was cancelled' }
//
// This is a DIFFERENT 503 from the admission-gate refusal ('spawn maintenance
// is quiescing', emitted when run() is called after quiesce) — this one is
// admitted-then-quiesced mid-flight, and it carries compensation. Slice 6
// converts spawn() to an Effect core; this guard, its compensate call, and its
// releaseCloneSlot/releaseTarget cleanup all live inside the coarse `runSpawn`
// tail (the try whose `finally` runs guard.settle()), and must survive the
// conversion byte-for-byte.
//
// Reaching 2084 with isOpen()===false is deterministic in-process. The ONLY
// pre-2084 clone-path await that a PATH git-shim can interpose is
// probeRepoAccess (`git ls-remote`, spawns.ts:1738 — reached with no abort
// signal). The shim parks there; the test quiesces maintenance while it is
// parked (provisioningOps is still empty, so quiesce's abort loop is a no-op),
// then releases the shim. probeRepoAccess returns ok, control runs
// synchronously through reserveCloneSlot + createSpawnedCard +
// insertProvisionalSpawn to the isOpen() re-read, which is now false.
//
// Parameterized over the two composers: `runControlDetached` injected walks
// spawnEffect; runner ABSENT walks spawnLegacy (spawns.ts dispatcher). Both
// must produce identical 503 wire bytes + gone + 1× SpawnCancelled.
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

// Frozen pre-abort 503 — both composers must emit these exact bytes.
const QUIESCE_CANCEL_WIRE = {
  status: 503,
  body: { ok: false, reason: 'daemon is shutting down; spawn was cancelled' },
} as const;
const QUIESCE_CANCEL_BODY_BYTES =
  '{"ok":false,"reason":"daemon is shutting down; spawn was cancelled"}';

// A spawn admitted while OPEN, parked at probeRepoAccess, then overtaken by a
// quiesce reaches spawns.ts:2084 with isOpen()===false and compensates.
async function runAdmittedThenQuiesced(
  t: TestContext,
  runner: typeof runControlDetached | undefined,
): Promise<{
  out: ControlResult;
  spawn_id: string;
  status: string | undefined;
  cancelled: number;
}> {
  const scratch = mkdtempSync(path.join(tmpdir(), 'fleetdeck-spawn-quiesce-'));
  const shimDir = path.join(scratch, 'bin');
  const reposDir = path.join(scratch, 'repos');
  const started = path.join(scratch, 'ls-remote-started');
  const release = path.join(scratch, 'ls-remote-release');
  mkdirSync(shimDir, { recursive: true });
  const gitShim = path.join(shimDir, 'git');
  writeFileSync(
    gitShim,
    [
      '#!/bin/sh',
      'case "$1" in',
      // validateBranch's `git check-ref-format --branch main` — pass it through.
      '  check-ref-format) exit 0 ;;',
      // probeRepoAccess's `git ls-remote -- <origin> HEAD` — the interposition
      // point. Announce readiness, then park until the test releases us so the
      // quiesce can land while spawn() is still before its isOpen() re-read.
      '  ls-remote)',
      '    printf started > "$FD_LSREMOTE_STARTED"',
      '    while [ ! -f "$FD_LSREMOTE_RELEASE" ]; do sleep 0.05; done',
      '    exit 0 ;;',
      // A clone must never run: the 503 returns before the provisioning fork.
      '  clone) exit 1 ;;',
      'esac',
      'exit 0',
      '',
    ].join('\n'),
  );
  chmodSync(gitShim, 0o755);
  setEnv(t, {
    PATH: `${shimDir}:${process.env['PATH'] ?? ''}`,
    FLEETDECK_REPOS_DIR: reposDir,
    FD_LSREMOTE_STARTED: started,
    FD_LSREMOTE_RELEASE: release,
  });

  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 4711,
    home: scratch,
    tmuxAdapter: makeAdapter(),
    // Injected → spawnEffect; omitted → spawnLegacy. Same producer (spawnStep).
    ...(runner ? { runControlDetached: runner } : {}),
  });
  // Teardown runs in registration (FIFO) order: release any still-parked shim
  // and join the owned maintenance BEFORE removing the scratch tree, so the
  // shim's release file is still writable while a stray fiber might read it.
  t.after(async () => {
    if (existsSync(scratch) && !existsSync(release)) writeFileSync(release, 'go');
    await core.lifecycle.close();
    db.close();
  });
  t.after(() => rmSync(scratch, { recursive: true, force: true }));

  // Admitted while OPEN — parks at probeRepoAccess (ls-remote).
  const spawnPromise = core.spawn({
    repo: 'https://example.com/fleetdeck-spawn-quiesce.git',
    branch: 'main',
    branch_mode: 'in-place',
  }) as Promise<ControlResult>;
  await waitUntil(() => existsSync(started), {
    timeoutMs: 8_000,
    intervalMs: 10,
    label: 'ls-remote preflight to park',
  });

  // Quiesce while the preflight is parked: maintenancePhase flips to
  // 'quiescing' (isOpen()===false) with provisioningOps still empty.
  // spawnCapability().available mirrors spawnMaintenance.isOpen() on the surface.
  // The core interface types spawnCapability() as `() => unknown` (it feeds the
  // /health + /state JSON), so narrow its runtime shape locally for the assert.
  const capAvailable = (): boolean => (core.spawnCapability() as { available: boolean }).available;
  assert.equal(capAvailable(), true, 'admitted while maintenance was open');
  assert.equal(core.spawnLifecycle.quiesce(), true, 'quiesce flipped the phase open → quiescing');
  assert.equal(capAvailable(), false, 'quiesced before the pre-abort re-read');

  // Release the preflight; control runs to the isOpen() re-read, now false.
  writeFileSync(release, 'go');
  const out = await spawnPromise;
  const spawn_id = db
    .prepare<{ spawn_id: string }>('SELECT spawn_id FROM spawns LIMIT 1')
    .get()?.spawn_id;
  assert.ok(spawn_id, 'the provisional row was durable before the guard fired');
  return {
    out,
    spawn_id,
    status: spawnStatus(db, spawn_id),
    cancelled: countEvents(db, 'SpawnCancelled'),
  };
}

const CORE_VARIANTS = [
  { label: 'effect', runner: runControlDetached },
  { label: 'legacy', runner: undefined },
] as const;

for (const variant of CORE_VARIANTS) {
  test(`spawn quiesce-cancel (${variant.label}): admitted-then-quiesced at the pre-abort guard → 503 + compensated`, async (t) => {
    const result = await runAdmittedThenQuiesced(t, variant.runner);
    assert.deepEqual(
      result.out,
      QUIESCE_CANCEL_WIRE,
      `${variant.label}: the pre-abort quiesce guard returns the admitted-then-quiesced 503 wire`,
    );
    assert.equal(
      JSON.stringify(result.out.body),
      QUIESCE_CANCEL_BODY_BYTES,
      `${variant.label}: identical 503 wire bytes`,
    );
    assert.equal(
      result.status,
      'gone',
      `${variant.label}: compensation tombstoned the provisional row`,
    );
    assert.equal(result.cancelled, 1, `${variant.label}: exactly one SpawnCancelled tombstone`);
  });
}
