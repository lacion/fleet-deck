import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { TestContext } from 'node:test';

import { openDb } from '../src/daemon/db.ts';
import { createCore } from '../src/daemon/derive.ts';
import test from './helpers/harness-test.ts';

// P9.1 Slice 1 characterization — BUG-040 plan-claim compensation.
//
// The invariant: a spawn carrying `plan_id` flips the plan
// proposed|approved|captured -> executed in ONE guarded UPDATE
// (claimPlanExecution) BEFORE any clone/worktree/pane/durable row exists. Every
// spawn exit that is not an explicit "complete" must RELEASE that claim —
// restore the pre-claim status — and the release is via-keyed
// (releasePlanExecution ... AND executed_via=?), so a concurrent archive/mark
// that re-stamps executed_via in the failure window is never clobbered.
//
// This file pins the invariant against a throw that escapes launchPane AFTER the
// claim (a synchronous launchOverride throw — the "throw escaping launchPane"
// case spawn() names), which the legacy `wrapSpawnFailure` closure already
// catches-and-releases. Slice 1 extracts releasePlanClaim / completePlanClaim /
// wrapSpawnFailure into a structural, ensuring-style combinator; per the spec
// this test is authored against the LEGACY code and must pass UNCHANGED after
// the refactor. It is characterization, not new behaviour.

type CoreTmuxAdapter = NonNullable<NonNullable<Parameters<typeof createCore>[1]>['tmuxAdapter']>;

interface PlanStateRow {
  status: string;
  executed_via: string | null;
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

function insertApprovedPlan(db: ReturnType<typeof openDb>): number {
  const info = db
    .prepare(
      `INSERT INTO plans (session_id, callsign, plan_md, created_at, status)
       VALUES ('planner-sid', 'otter-plan', '# plan', ?, 'approved')`,
    )
    .run(1_700_000_000_000);
  return Number(info.lastInsertRowid);
}

function readPlan(db: ReturnType<typeof openDb>, planId: number): PlanStateRow {
  const row = db
    .prepare<PlanStateRow>('SELECT status, executed_via FROM plans WHERE plan_id = ?')
    .get(planId);
  assert.ok(row, 'plan row exists');
  return row;
}

test('plan-claim compensation: a throw during launch reverts the plan to its pre-claim status (via-keyed)', async (t: TestContext) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plan-claim-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const db = openDb(':memory:');
  const core = createCore(db, {
    port: 4711,
    home: cwd,
    tmuxAdapter: makeAdapter({
      spawnOverrideCmd: () => '/fake-spawn-override',
      launchOverride: () => {
        throw new Error('launch override boom');
      },
    }),
  });
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  const planId = insertApprovedPlan(db);
  const before = readPlan(db, planId);
  assert.equal(before.status, 'approved');
  assert.equal(before.executed_via, null);

  await assert.rejects(
    () => core.spawn({ cwd, prompt: 'do the thing', plan_id: planId }) as Promise<unknown>,
    /launch override boom/,
    'a throw escaping launchPane must propagate out of spawn()',
  );

  const after = readPlan(db, planId);
  assert.equal(
    after.status,
    'approved',
    'the failed spawn released the claim back to the pre-claim status',
  );
  assert.match(
    after.executed_via ?? '',
    /^spawn:[0-9a-f]{8}$/,
    "release is via-keyed: the claim's own via is what the release matched on (and left recorded)",
  );
});

test('plan-claim compensation: a concurrent via change in the failure window is not clobbered (via-match guard)', async (t: TestContext) => {
  const cwd = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plan-claim-vm-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const db = openDb(':memory:');
  const planId = (() => {
    const info = db
      .prepare(
        `INSERT INTO plans (session_id, callsign, plan_md, created_at, status)
         VALUES ('planner-sid', 'otter-plan', '# plan', ?, 'approved')`,
      )
      .run(1_700_000_000_000);
    return Number(info.lastInsertRowid);
  })();
  const core = createCore(db, {
    port: 4711,
    home: cwd,
    tmuxAdapter: makeAdapter({
      spawnOverrideCmd: () => '/fake-spawn-override',
      launchOverride: () => {
        // A concurrent actor (another claim / archive-mark) re-stamps the plan's
        // executed_via inside this spawn's failure window, then the launch fails.
        // The via-keyed release must NOT revert a row it no longer owns.
        db.prepare('UPDATE plans SET executed_via = ? WHERE plan_id = ?').run(
          'spawn:concurrent',
          planId,
        );
        throw new Error('launch override boom');
      },
    }),
  });
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  await assert.rejects(
    () => core.spawn({ cwd, prompt: 'do the thing', plan_id: planId }) as Promise<unknown>,
    /launch override boom/,
  );

  const after = readPlan(db, planId);
  assert.equal(
    after.status,
    'executed',
    'via-match guard: the release did not revert a row a concurrent actor re-claimed',
  );
  assert.equal(after.executed_via, 'spawn:concurrent', 'the concurrent via is preserved');
});

// P9.1 §7 — BINDING PRE-SLICE-6 REQUIREMENT (docs/v1/evidence/effect/p9-1-design.md).
//
// Before /api/spawn moves onto the P6.4 transport (Slice 6a) the plan-claim
// compensation must be pinned against the class of 400 the design flags as the
// riskiest to regress: a *repo-mode validation refusal that fires AFTER the
// claim*. Unlike the two throws above, these are RETURNED wires — the spawn body
// resolves with {status:400, body} rather than rejecting — and each of them
// lives inside the try whose `finally` runs `guard.settle()`. So the invariant
// under test is exactly the same one Slice 6a must preserve byte-for-byte: the
// plan flipped approved -> executed by the claim, then a repo-mode 400 returned,
// and `guard.settle()` reverted the plan back to its pre-claim status with the
// claim's own via still recorded (via-keyed release). Each case runs the
// PRODUCTION path — core.spawn (ownedSpawn) with the tmux runner injected — and
// asserts BOTH the wire bytes and the reverted plan row. Authored against the
// legacy code; must pass UNCHANGED after Slice 6a wires the Effect transport.
//
// A plain repo body reaches the claim block (spawnCapability available via the
// test-override; runtimeOverrideRefusal / unsupervisedGate / gatewayDecision all
// pass a bare repo request) and then the three repo-mode gates, every one of
// which returns BEFORE validateBranch/resolveTarget — so no repo resolution or
// clone is attempted, and the tmux adapter is never driven past capability.

type SpawnWire = { status: number; body: { ok: boolean; reason: string } };

function makeRepoModeCore(db: ReturnType<typeof openDb>, home: string) {
  return createCore(db, {
    port: 4711,
    home,
    tmuxAdapter: makeAdapter({ spawnOverrideCmd: () => '/fake-spawn-override' }),
  });
}

test('plan-claim compensation §7: a repo-mode worktree-conflict 400 reverts the claim (via-keyed)', async (t: TestContext) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plan-claim-repo-wt-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const db = openDb(':memory:');
  const core = makeRepoModeCore(db, home);
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  const planId = insertApprovedPlan(db);
  assert.equal(readPlan(db, planId).status, 'approved');

  const out = (await core.spawn({
    repo: 'owner/repo',
    worktree: true,
    plan_id: planId,
  })) as SpawnWire;
  assert.equal(out.status, 400);
  assert.equal(out.body.ok, false);
  assert.equal(out.body.reason, 'branch_mode replaces worktree in repo mode');

  const after = readPlan(db, planId);
  assert.equal(
    after.status,
    'approved',
    'a repo-mode 400 after the claim released it back to the pre-claim status',
  );
  assert.match(
    after.executed_via ?? '',
    /^spawn:[0-9a-f]{8}$/,
    "release is via-keyed: the claim's own via is recorded",
  );
});

test('plan-claim compensation §7: a repo-mode branch-required 400 reverts the claim (via-keyed)', async (t: TestContext) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plan-claim-repo-br-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const db = openDb(':memory:');
  const core = makeRepoModeCore(db, home);
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  const planId = insertApprovedPlan(db);
  assert.equal(readPlan(db, planId).status, 'approved');

  const out = (await core.spawn({ repo: 'owner/repo', plan_id: planId })) as SpawnWire;
  assert.equal(out.status, 400);
  assert.equal(out.body.ok, false);
  assert.equal(out.body.reason, 'branch is required in repo mode');

  const after = readPlan(db, planId);
  assert.equal(
    after.status,
    'approved',
    'a repo-mode 400 after the claim released it back to the pre-claim status',
  );
  assert.match(after.executed_via ?? '', /^spawn:[0-9a-f]{8}$/, 'release is via-keyed');
});

test('plan-claim compensation §7: a repo-mode invalid-branch_mode 400 reverts the claim (via-keyed)', async (t: TestContext) => {
  const home = mkdtempSync(path.join(tmpdir(), 'fleetdeck-plan-claim-repo-bm-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  const db = openDb(':memory:');
  const core = makeRepoModeCore(db, home);
  t.after(async () => {
    await core.lifecycle.close();
    db.close();
  });

  const planId = insertApprovedPlan(db);
  assert.equal(readPlan(db, planId).status, 'approved');

  const out = (await core.spawn({
    repo: 'owner/repo',
    branch: 'main',
    branch_mode: 'bogus',
    plan_id: planId,
  })) as SpawnWire;
  assert.equal(out.status, 400);
  assert.equal(out.body.ok, false);
  assert.equal(out.body.reason, 'branch_mode must be worktree or in-place');

  const after = readPlan(db, planId);
  assert.equal(
    after.status,
    'approved',
    'a repo-mode 400 after the claim released it back to the pre-claim status',
  );
  assert.match(after.executed_via ?? '', /^spawn:[0-9a-f]{8}$/, 'release is via-keyed');
});
