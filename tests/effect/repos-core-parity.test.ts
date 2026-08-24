// tests/effect/repos-core-parity.test.ts — P9.2 Slice 2 CORE parity pin.
//
// P9.2 Slice 2 moves POST /api/repos/preflight's body (preflightRepo) behind a
// ZERO-GATE Effect core dispatched by the ctx-resident runControlDetached. This
// pin fixes the one property the CORE half hangs on that the transport suite
// cannot see: a REAL dispatcher rejection (not a stubbed core.preflightRepo)
// induced at the git/exec seam, flag-true vs runner-absent.
//
// The dispatcher chooses the Effect core iff `EFFECT_CORE_REPOS_PREFLIGHT &&
// runControlDetached` (repos.ts §preflightRepo). So the two arms below are the
// two branches the flag/injection toggle:
//   - Effect arm: a real runControlDetached is injected -> preflightRepoEffect
//                 runs (the EFFECT_CORE_REPOS_PREFLIGHT = true production path).
//   - legacy arm: NO runner injected                    -> preflightRepoLegacy
//                 runs (byte-identical to the flag = false rollback).
//
// §6 defect-identity (BINDING): a runPreflightRepo rejection (probeRepoAccess's
// `git ls-remote` via execFileP mocked to reject) must surface with the RAW
// error, identity-preserved, on BOTH arms. On the Effect arm this is
// Effect.promise-die -> runControlDetached causeSquash; on the legacy arm it is
// the async body's own rejection. The transport 500 fold (legacy `.catch` AND
// repoPreflightWorkflow's onError) then sees the IDENTICAL object and emits the
// DISTINCT preflight 500 + log on either path only because this holds.
//
// Stubbing core.preflightRepo (http-workflow-repos C) REPLACES the dispatcher,
// so C never sees the squash. D only drives a caught resolveTarget 400 (the
// promise RESOLVES). This file is the missing per-route dispatcher-rejection
// pin §6 asked for.
import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spyOn } from 'bun:test';
import * as Effect from 'effect/Effect';
import * as exec from '../../src/daemon/exec.ts';
import { createRepos } from '../../src/daemon/repos.ts';
import { type RunControlDetached } from '../../src/daemon/retention.ts';
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import test from '../helpers/harness-test.ts';

type Repos = ReturnType<typeof createRepos>;

// createRepos needs only these slivers of ctx (mirrors http-workflow-repos D):
// an empty catalog and no persisted settings. The optional runControlDetached
// is the slice-2 seam — present ⇒ the Effect core runs.
function fakePreflightCtx(runner?: RunControlDetached) {
  const base = {
    q: {
      repoByName: {
        all: () => [] as { repo_name: string; root: string; origin_url: string | null }[],
      },
      getSetting: { get: () => undefined },
    },
    onMutate: () => {
      /* test stub */
    },
  };
  return (runner ? { ...base, runControlDetached: runner } : base) as unknown as Parameters<
    typeof createRepos
  >[0];
}

function makeRepos(withRunner: boolean, runner: RunControlDetached = runControlDetached): Repos {
  return createRepos(fakePreflightCtx(withRunner ? runner : undefined));
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

// Simulated transport fold (http.ts legacy `.catch` AND repoPreflightWorkflow's
// onError + 500 SUCCESS wire). The real settlers write JSON onto the response;
// we record which Promise arm fired so a dropped causeSquash (FiberFailure
// wrapper, or a DATA 400 from resolveTarget catching the spy) shows up as
// via:'then' instead of via:'catch' + the DISTINCT preflight 500.
const PREFLIGHT_LOG = 'fleetd repo preflight error:';
const PREFLIGHT_500_BODY = { ok: false, reason: 'Git access check failed internally' };

function settlePreflight(operation: Promise<{ status: number; body: unknown }>): Promise<{
  via: 'then' | 'catch';
  status: number;
  body: unknown;
  err: unknown;
}> {
  return operation.then(
    (out) => ({ via: 'then' as const, status: out.status, body: out.body, err: undefined }),
    (err: unknown) => {
      console.error(PREFLIGHT_LOG, err);
      return { via: 'catch' as const, status: 500, body: PREFLIGHT_500_BODY, err };
    },
  );
}

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

function preflightLogs(errors: unknown[][]): unknown[][] {
  return errors.filter((a) => a[0] === PREFLIGHT_LOG);
}

// Point FLEETDECK_REPOS_DIR at a fresh empty temp dir so resolveTarget's dest
// does not exist on disk: a URL target then returns clone-mode WITHOUT calling
// gitRepoKind (which also uses execFileP). The spy therefore fires at
// probeRepoAccess's `git ls-remote` — the git/exec seam runPreflightRepo does
// NOT catch (resolveTarget's try/catch is the 400 DATA arm).
function withTempReposDir(): string {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'fd-p92-repos-core-parity-'));
  process.env['FLEETDECK_REPOS_DIR'] = dir;
  return dir;
}

function restoreReposDir(dir: string, prev: string | undefined): void {
  rmSync(dir, { recursive: true, force: true });
  if (prev === undefined) delete process.env['FLEETDECK_REPOS_DIR'];
  else process.env['FLEETDECK_REPOS_DIR'] = prev;
}

const CLONE_URL = 'https://github.com/octocat/fd-p92-repos-core-parity-no-such-repo';

test('preflightRepo() reject parity: an execFileP ls-remote rejection surfaces RAW and identical on both arms, and the transport fold produces the identical 500 + log (§6 defect-identity)', async (t) => {
  const prevDir = process.env['FLEETDECK_REPOS_DIR'];
  const reposDir = withTempReposDir();
  t.after(() => restoreReposDir(reposDir, prevDir));

  // Inject at the git/exec seam: probeRepoAccess awaits execFileP('git',
  // ['ls-remote', ...]). Mocking it to reject makes runPreflightRepo reject on
  // BOTH arms — the Effect arm via Effect.promise-die -> runControlDetached
  // reject, the legacy arm via the async body's own rejection.
  // files-core-parity / worktrees-core-parity spy the same way.
  const sentinel = new Error('repos-core-parity ls-remote sentinel');
  const spy = spyOn(exec, 'execFileP').mockImplementation(((
    cmd: string,
    args: readonly string[] = [],
  ) => {
    if (cmd === 'git' && args[0] === 'ls-remote') return Promise.reject(sentinel);
    return Promise.reject(new Error(`unexpected execFileP: ${cmd} ${JSON.stringify(args)}`));
  }) as typeof exec.execFileP);
  t.after(() => spy.mockRestore());

  let n = 0;
  const wrapping: RunControlDetached = (effect) => {
    n += 1;
    return runControlDetached(effect);
  };

  const body = { repo: CLONE_URL };

  // RAW rejection identity (the dispatcher, before any transport fold).
  const rawEff = await rejectionOf(makeRepos(true, wrapping).preflightRepo(body));
  const rawLeg = await rejectionOf(makeRepos(false, wrapping).preflightRepo(body));
  assert.equal(rawEff, sentinel, 'Effect arm must reject with the RAW error by identity');
  assert.equal(rawLeg, sentinel, 'legacy arm must reject with the RAW error by identity');
  assert.equal(rawEff, rawLeg, 'both arms must reject with the SAME error object');
  assert.equal(n, 1, 'one preflightRepo() on the Effect factory must discharge the runner');
  const afterRaw = n;
  // The runner-absent call above must not have incremented n (makeRepos(false)
  // omits the wrapper). A second Effect call would be n=2; pin the gap.
  assert.equal(afterRaw, 1, 'runner-absent factory must not invoke the injected runner');

  // Transport fold: identical 500 wire + log with raw-error identity on both
  // arms. This is the fold the HTTP route's `.catch` / workflow onError apply;
  // pinning it here means a FiberFailure wrapper would show up as a log that
  // is NOT `=== sentinel`.
  const expected = {
    via: 'catch' as const,
    status: 500,
    body: PREFLIGHT_500_BODY,
    err: sentinel,
  };

  n = 0;
  const effFold = await withErrorLog(() =>
    settlePreflight(makeRepos(true, wrapping).preflightRepo(body)),
  );
  const legFold = await withErrorLog(() =>
    settlePreflight(makeRepos(false, wrapping).preflightRepo(body)),
  );

  assert.deepEqual(effFold.result, expected, 'Effect arm folds to the DISTINCT preflight 500');
  assert.deepEqual(legFold.result, expected, 'legacy arm folds to the DISTINCT preflight 500');
  assert.equal(effFold.result.err, sentinel, 'Effect fold logs/catches the RAW error by identity');
  assert.equal(legFold.result.err, sentinel, 'legacy fold logs/catches the RAW error by identity');
  assert.deepEqual(
    preflightLogs(effFold.errors),
    [[PREFLIGHT_LOG, sentinel]],
    'Effect fold logs the preflight line once with the RAW sentinel',
  );
  assert.deepEqual(
    preflightLogs(legFold.errors),
    [[PREFLIGHT_LOG, sentinel]],
    'legacy fold logs the preflight line once with the RAW sentinel',
  );
  assert.equal(n, 1, 'fold-path Effect factory discharges the runner once (legacy adds 0)');
});

test('injected runControlDetached is invoked on the Effect factory and skipped when absent', async () => {
  // Equality of the two factory wires cannot see a dead dispatcher that always
  // took preflightRepoLegacy (legacy IS the characterization). Wrapping the
  // injected runner locks the seam: one preflightRepo() must fan out n=1 on the
  // Effect factory and leave the counter unchanged on the runner-absent factory.
  // Driven with the network-free empty-repo 400 (parseRepoInput rejects before
  // any catalog/git/env lookup) so this pin does not depend on the exec spy.
  const EXPECT = { status: 400, body: { ok: false, reason: 'repo must be a non-empty string' } };

  let n = 0;
  const wrapping: RunControlDetached = (effect) => {
    n += 1;
    return runControlDetached(effect);
  };

  const wiredOut = await makeRepos(true, wrapping).preflightRepo({ repo: '' });
  assert.equal(n, 1, 'one preflightRepo() on the Effect factory must discharge through the runner');
  assert.deepEqual(wiredOut, EXPECT);

  const afterEffect = n;
  const legacyOut = await makeRepos(false, wrapping).preflightRepo({ repo: '' });
  assert.equal(
    n,
    afterEffect,
    'runner-absent factory must not invoke the injected runner (n unchanged, expected 0 more)',
  );
  assert.deepEqual(legacyOut, EXPECT);
  assert.equal(afterEffect, 1);
});

test('runControlDetached surfaces a died Effect as the RAW defect (§6 Q4 seam identity)', async () => {
  // Same seam worktrees-core-parity / files-core-parity pin: Effect.promise of a
  // rejected runPreflightRepo dies, and runPromiseWith rejects with
  // causeSquash(exit.cause) — the raw defect, identity-preserved, with NO
  // FiberFailure wrapper. The transport 500 fold then catches this raw
  // rejection.
  const sentinel = new Error('repos-core-parity Q4 sentinel');
  let caught: unknown;
  try {
    await runControlDetached(Effect.promise<never>(() => Promise.reject(sentinel)));
    assert.fail('a died Effect must reject the discharged promise');
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, sentinel, 'the rejection must be the RAW defect by identity (no wrapper)');
  assert.ok(caught instanceof Error);
  assert.equal((caught as Error).message, 'repos-core-parity Q4 sentinel');
});
