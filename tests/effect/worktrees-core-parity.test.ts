// tests/effect/worktrees-core-parity.test.ts — P9.2 Slice 1 CORE parity pin.
//
// P9.2 Slice 1 moves the GET /api/worktrees inspector body (worktrees()) behind a
// ZERO-GATE Effect core dispatched by the ctx-resident runControlDetached. This
// pin fixes the one property the CORE half hangs on: the Effect core is a
// byte-for-byte substitute for the legacy async body across BOTH the success wire
// (a real worktree fan-out) and the rejection wire — with no divergence in console
// output ("log dialect").
//
// The dispatcher chooses the Effect core iff `EFFECT_CORE_WORKTREES_READ &&
// runControlDetached` (worktrees.ts §worktrees). So the two arms below are the two
// branches the flag/injection toggle:
//   - Effect arm: a real runControlDetached is injected -> worktreesReadEffect runs
//                 (the EFFECT_CORE_WORKTREES_READ = true production path).
//   - legacy arm: NO runner injected                    -> worktreesLegacy runs
//                 (byte-identical to the flag = false rollback, and to every
//                  standalone-factory caller in worktrees.test.ts).
// Asserting the two arms agree wire-for-wire and log-for-log pins the flag's two
// positions as observationally identical — the rollback twin is safe.
//
// Two follow-up pins lock the seams equality cannot see:
//   - Reject parity: a runWorktreesRead rejection (mapLimit mocked to reject) must
//     surface with the RAW error, identity-preserved, on BOTH arms. On the Effect
//     arm this is Effect.promise-die -> runControlDetached reject (§6 Q4); on the
//     legacy arm it is the async body's own rejection. The transport fail-soft fold
//     folds an IDENTICAL rejection on either path only because this holds.
//   - Dispatcher liveness: wrapping the injected runner must observe n=1 for one
//     worktrees() on the Effect factory and n unchanged without a runner, so a
//     ternary that always took *Legacy cannot green this file.
// The last case pins the §6 Q4 ruling directly at the seam: a died Effect
// discharged through the production runControlDetached rejects with the RAW defect
// (identity-preserved), never a FiberFailure wrapper — the exact mechanism the
// transport fail-soft fold depends on.
import assert from 'node:assert/strict';
import { spyOn } from 'bun:test';
import * as Effect from 'effect/Effect';
import { openDb } from '../../src/daemon/db.ts';
import * as helpers from '../../src/daemon/helpers.ts';
import { createKeyedMutex } from '../../src/daemon/helpers.ts';
// Precedent (files-core-parity.test.ts:46): tests import the PRODUCTION detached
// runner from the platform module and inject it, exercising the real
// Effect.runPromiseWith(Context.empty()) seam rather than a stand-in.
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { createStatements } from '../../src/daemon/statements.ts';
import type { SqliteHandle } from '../../src/daemon/sqlite.ts';
import { createWorktrees } from '../../src/daemon/worktrees.ts';
import { makeRepoWithWorktree } from '../helpers/gitrepo.ts';
import test from '../helpers/harness-test.ts';

type Worktrees = ReturnType<typeof createWorktrees>;
type Statements = ReturnType<typeof createStatements>['q'];

// createWorktrees' tick/onMutate are irrelevant to inspection; a shared no-op
// stands in (an empty method body trips @typescript-eslint/no-empty-function).
const noop = (): void => {
  /* test stub */
};

// Seed a real fleet worktree row (session + spawn) so q.worktreeSpawns.all()
// returns the fixture — the SAME durable evidence ownWorktree writes in
// worktrees.test.ts, inlined onto a passed-in handle so both parity arms read one db.
function seedWorktree(db: SqliteHandle, repo: { worktree: string; root: string }): void {
  const now = Date.now();
  db.prepare(
    `INSERT INTO sessions
      (session_id, callsign, cwd, branch, col, note, events, started_at, last_seen, ended_at, archived_at, source)
      VALUES (?, 'otter', ?, 'wt-branch', 'offline', 'core parity', 0, ?, ?, ?, ?, 'spawned')`,
  ).run('worktree-session', repo.worktree, now, now, now, now);
  db.prepare(
    `INSERT INTO spawns
      (spawn_id, session_id, callsign, tmux_session, tmux_window, cwd, worktree_path, requested_at, status)
      VALUES (?, 'worktree-session', 'otter', 'fleetdeck-test', 'fd-otter', ?, ?, ?, 'pane-dead')`,
  ).run('spawn-worktree-session', repo.root, repo.worktree, now);
}

// Build a worktrees factory over `q`/`db`. With `withRunner`, the ctx carries the
// real runControlDetached (Effect-core arm); without it, the dispatcher's ternary
// falls through to worktreesLegacy (rollback arm) — identical ctx otherwise.
// `runner` lets a test wrap the production function and count discharges. Each
// factory gets its own keyed mutex, exactly like worktrees.test.ts's freshMutexCtx.
function makeWorktrees(
  q: Statements,
  db: SqliteHandle,
  withRunner: boolean,
  runner: typeof runControlDetached = runControlDetached,
): Worktrees {
  return createWorktrees({
    q,
    db,
    tick: noop,
    onMutate: noop,
    acquireWorktreePathLock: createKeyedMutex(),
    ...(withRunner ? { runControlDetached: runner } : {}),
  });
}

// Console capture: the inspector never logs, but the pin is "identical log
// dialect", so we record every console channel around each dispatch and assert the
// two arms produce the SAME (here empty) transcript.
const CONSOLE_METHODS = ['log', 'info', 'warn', 'error', 'debug', 'trace'] as const;
type ConsoleMethod = (typeof CONSOLE_METHODS)[number];

async function withConsoleCapture<T>(fn: () => Promise<T>): Promise<{ result: T; logs: string[] }> {
  const logs: string[] = [];
  const original = {} as Record<ConsoleMethod, (typeof console)['log']>;
  for (const m of CONSOLE_METHODS) {
    original[m] = console[m];
    console[m] = ((...args: unknown[]) => {
      logs.push(`${m}:${args.map((a) => String(a)).join(' ')}`);
    }) as (typeof console)['log'];
  }
  try {
    return { result: await fn(), logs };
  } finally {
    for (const m of CONSOLE_METHODS) console[m] = original[m];
  }
}

// Capture a promise's rejection value (identity-preserved), or a sentinel marking
// that it unexpectedly resolved.
const NO_REJECTION = Symbol('no-rejection');
async function rejectionOf(p: Promise<unknown>): Promise<unknown> {
  try {
    await p;
    return NO_REJECTION;
  } catch (err) {
    return err;
  }
}

test('worktrees() Effect core matches the legacy body on the success wire over a real worktree fixture', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-worktrees-core-parity' });
  t.after(() => repo.cleanup());
  const db = openDb(':memory:');
  t.after(() => db.close());
  seedWorktree(db, repo);
  const { q } = createStatements(db);

  // Both arms read the SAME untouched on-disk repo, back to back; the inspector
  // reads git state, never wall-clock, so the two wires are byte-identical.
  const eff = await withConsoleCapture(() => makeWorktrees(q, db, true).worktrees());
  const leg = await withConsoleCapture(() => makeWorktrees(q, db, false).worktrees());

  assert.deepEqual(
    eff.result,
    leg.result,
    'Effect-core inspector wire must equal the legacy wire byte-for-byte',
  );
  assert.deepEqual(eff.logs, leg.logs, 'console transcripts must match');
  assert.deepEqual(eff.logs, [], 'the inspector dispatch must not log on success');

  // Sanity: the fixture really produced a non-degenerate inspection (one worktree
  // that took the real git fan-out), so the parity above is meaningful.
  const body = eff.result as { ok: boolean; worktrees: Array<{ path: string; verdict: string }> };
  assert.equal(body.ok, true);
  assert.equal(body.worktrees.length, 1, 'the seeded spawn must inspect as exactly one worktree');
  assert.equal(body.worktrees[0]?.path, repo.worktree);
  assert.equal(body.worktrees[0]?.verdict, 'safe', 'a clean seeded worktree inspects as safe');
});

test('worktrees() reject parity: a runWorktreesRead rejection surfaces RAW and identical on both arms (§6 defect-identity)', async (t) => {
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-worktrees-core-parity-reject' });
  t.after(() => repo.cleanup());
  const db = openDb(':memory:');
  t.after(() => db.close());
  seedWorktree(db, repo);
  const { q } = createStatements(db);

  // Inject at the core tail: mapLimit is the shared helper worktrees() awaits, so
  // mocking it to reject makes runWorktreesRead reject on BOTH arms — the Effect
  // arm via Effect.promise-die -> runControlDetached reject, the legacy arm via the
  // async body's own rejection. files-core-parity spies exec.execFileP the same way.
  const sentinel = new Error('worktrees-core-parity reject sentinel');
  const spy = spyOn(helpers, 'mapLimit').mockImplementation((() =>
    Promise.reject(sentinel)) as typeof helpers.mapLimit);
  t.after(() => spy.mockRestore());

  const eff = await withConsoleCapture(() => rejectionOf(makeWorktrees(q, db, true).worktrees()));
  const leg = await withConsoleCapture(() => rejectionOf(makeWorktrees(q, db, false).worktrees()));

  assert.equal(eff.result, sentinel, 'Effect arm must reject with the RAW error by identity');
  assert.equal(leg.result, sentinel, 'legacy arm must reject with the RAW error by identity');
  assert.equal(eff.result, leg.result, 'both arms must reject with the SAME error object');
  assert.ok(sentinel instanceof Error);
  assert.equal((eff.result as Error).message, 'worktrees-core-parity reject sentinel');
  // A rejection is folded by the TRANSPORT, not the core, so the core itself is
  // silent on both arms (the inspector log fires only in http.ts).
  assert.deepEqual(eff.logs, [], 'the Effect core must not log the rejection');
  assert.deepEqual(leg.logs, [], 'the legacy core must not log the rejection');
});

test('injected runControlDetached is invoked on the Effect factory and skipped when absent', async (t) => {
  // Equality of the two factory wires cannot see a dead dispatcher that always took
  // worktreesLegacy (legacy IS the characterization). Wrapping the injected runner
  // locks the seam: one worktrees() must fan out n=1 on the Effect factory and
  // leave the counter unchanged on the runner-absent factory.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-worktrees-core-parity-live' });
  t.after(() => repo.cleanup());
  const db = openDb(':memory:');
  t.after(() => db.close());
  seedWorktree(db, repo);
  const { q } = createStatements(db);

  let n = 0;
  const wrapping = <A>(effect: Effect.Effect<A, never, never>): Promise<A> => {
    n += 1;
    return runControlDetached(effect);
  };

  await makeWorktrees(q, db, true, wrapping).worktrees();
  assert.equal(
    n,
    1,
    'one worktrees() on the Effect factory must discharge through the injected runner',
  );

  const afterEffect = n;
  await makeWorktrees(q, db, false, wrapping).worktrees();
  assert.equal(
    n,
    afterEffect,
    'runner-absent factory must not invoke the injected runner (n unchanged, expected 0 more)',
  );
  assert.equal(afterEffect, 1);
});

test('runControlDetached surfaces a died Effect as the RAW defect (§6 Q4 seam identity)', async () => {
  // §6 Q4 (BINDING): runPromiseWith rejects with causeSquash(exit.cause) — the raw
  // defect, identity-preserved, with NO FiberFailure wrapper. Effect.promise turns a
  // rejected promise into a DEFECT (die), which is exactly how worktrees()'s coarse
  // zero-gate tail surfaces a git fan-out rejection. The transport fail-soft fold
  // then catches this raw rejection — pin the mechanism here so the fold rests on
  // observed behavior.
  const sentinel = new Error('worktrees-core-parity Q4 sentinel');
  let caught: unknown;
  try {
    await runControlDetached(Effect.promise<never>(() => Promise.reject(sentinel)));
    assert.fail('a died Effect must reject the discharged promise');
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, sentinel, 'the rejection must be the RAW defect by identity (no wrapper)');
  assert.ok(caught instanceof Error);
  assert.equal((caught as Error).message, 'worktrees-core-parity Q4 sentinel');
});
