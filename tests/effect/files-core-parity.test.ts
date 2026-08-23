// tests/effect/files-core-parity.test.ts — P9.3 (slices 1+2) rejection-parity pin.
//
// P9.3 moves the files read core (readAt) and list core (listAt, incl. the
// git-ignore tail) behind a per-op Effect dispatcher discharged by the
// ctx-resident runControlDetached. This pin fixes the ONE property the whole
// slice hangs on: the Effect core is a byte-for-byte substitute for the legacy
// async body across BOTH success and every rejection wire — with no divergence
// in console output ("log dialect").
//
// The dispatcher chooses the Effect core iff `EFFECT_CORE_FILES && runFs`
// (files.ts §dispatchList/§dispatchRead). So the two arms below are the exact
// two branches the flag toggles:
//   - filesEffect: a real runControlDetached is injected  -> Effect core runs
//                  (this IS the EFFECT_CORE_FILES = true production path).
//   - filesLegacy: NO runner injected                     -> *Legacy body runs
//                  (byte-identical to the EFFECT_CORE_FILES = false rollback,
//                   and to every standalone-factory caller e.g.
//                   files-run-bounded's filesAt()).
// Asserting the two arms agree, wire-for-wire and log-for-log, pins the flag's
// two positions as observationally identical — the rollback twin is safe.
//
// Two follow-up pins lock the seams equality cannot see:
//   - D6 reject arm: a git tree whose ignoredPaths await REJECTS must still
//     settle via then as in-core 404 `{ok:false, reason:'not found'}` on BOTH
//     dispatch paths (runIgnore's catch vs listAtLegacy's single try), never a
//     transport 500. Dropping only the Effect catch would green every other case.
//   - Dispatcher liveness: wrapping the injected runner must observe n=2 for
//     one read+one list on the Effect factory and n unchanged without a runner,
//     so a ternary that always took *Legacy cannot green this file.
// The last case pins the §6 Q4 ruling directly at the seam: a died Effect
// discharged through the production runControlDetached rejects with the RAW
// defect (identity-preserved), never a FiberFailure wrapper. That is the exact
// mechanism by which search's uncaught git-ignore tail would surface a 500 in
// P9.5 — and, by contrast, why list's IN-CORE try/catch (D6) keeps its rejection
// a 404. We pin the seam here so the semantics are nailed independent of slice 3.
import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { spyOn } from 'bun:test';
import * as Effect from 'effect/Effect';
import * as exec from '../../src/daemon/exec.ts';
import { createFiles } from '../../src/daemon/files.ts';
// Precedent (tests/dismiss.test.ts:19): tests import the PRODUCTION detached
// runner from the platform module and inject it, exercising the real
// Effect.runPromiseWith(Context.empty()) seam rather than a stand-in.
import { runControlDetached } from '../../src/daemon/platform/bun/ingress-supervisor-live.ts';
import { makePlainDir, makeRepoWithWorktree } from '../helpers/gitrepo.ts';
import test from '../helpers/harness-test.ts';

type Files = ReturnType<typeof createFiles>;

// Build a files factory over `root`. With `withRunner`, the ctx carries the real
// runControlDetached (Effect-core arm); without it, the dispatcher's ternary
// falls through to the *Legacy bodies (rollback arm) — identical ctx otherwise.
// `runner` lets a test wrap the production function and count discharges.
function makeFiles(
  root: string,
  withRunner: boolean,
  runner: typeof runControlDetached = runControlDetached,
): Files {
  const ctx = {
    q: {
      getSession: { get: () => null },
      spawnBySession: { get: () => null },
    },
    browseRootChoice: () => ({ source: 'override' as const, resolved: root }),
    ...(withRunner ? { runControlDetached: runner } : {}),
  };
  return createFiles(ctx as unknown as Parameters<typeof createFiles>[0]);
}

// Console capture: read/list never log, but the pin is "identical log dialect",
// so we record every console channel around each dispatch and assert the two
// arms produce the SAME transcript (and, here, that it is empty).
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

interface ParityCase {
  label: string;
  run: (f: Files) => Promise<unknown>;
}

// Drive one case through BOTH arms over the SAME on-disk fixture and assert:
//   (1) byte-identical wire ({status, body}) — the whole point of the slice, and
//   (2) byte-identical console transcript, which must be empty for read/list.
async function assertParity(root: string, kase: ParityCase): Promise<void> {
  const eff = await withConsoleCapture(() => kase.run(makeFiles(root, true)));
  const leg = await withConsoleCapture(() => kase.run(makeFiles(root, false)));
  assert.deepEqual(
    eff.result,
    leg.result,
    `${kase.label}: Effect-core wire must equal the legacy wire byte-for-byte`,
  );
  assert.deepEqual(eff.logs, leg.logs, `${kase.label}: console transcripts must match`);
  assert.deepEqual(eff.logs, [], `${kase.label}: read/list dispatch must not log`);
}

// Simulated settleFilesystemOperation (http.ts:834-847). The real settler writes
// JSON onto the response; we record which Promise arm fired so a dropped
// runIgnore catch (Effect.promise die → runControlDetached reject) shows up as
// via:'catch' + 500 `{ok:false, reason:'internal'}` + console.error, while the
// in-core failure() path stays via:'then' + 404.
function settleFsOp(
  operation: Promise<{ status: number; body: unknown }>,
  scope: 'session' | 'home' = 'home',
): Promise<{ via: 'then' | 'catch'; status: number; body: unknown }> {
  return operation
    .then(({ status, body }) => ({ via: 'then' as const, status, body }))
    .catch((err: unknown) => {
      console.error(`fleetd ${scope} filesystem error:`, err);
      return { via: 'catch' as const, status: 500, body: { ok: false, reason: 'internal' } };
    });
}

test('files read+list Effect cores match the legacy bodies across success and rejection wires', async (t) => {
  // Non-git fixture: a readable text file, a subdirectory, nothing else.
  const plain = makePlainDir();
  t.after(() => plain.cleanup());
  writeFileSync(path.join(plain.dir, 'note.txt'), 'hello\nworld\n');
  mkdirSync(path.join(plain.dir, 'sub'));

  const nonGitCases: ParityCase[] = [
    // read — success + every rejection branch reachable in-core.
    { label: 'read 200 text file', run: (f) => f.fsReadHome('note.txt') },
    { label: 'read 404 root is-a-directory', run: (f) => f.fsReadHome('') },
    { label: 'read 404 subdir is-a-directory', run: (f) => f.fsReadHome('sub') },
    { label: 'read 404 missing', run: (f) => f.fsReadHome('nope.txt') },
    { label: 'read 400 traversal', run: (f) => f.fsReadHome('../escape') },
    { label: 'read 404 credential (.ssh)', run: (f) => f.fsReadHome('.ssh/id_rsa') },
    { label: 'read 404 .docker/config.json', run: (f) => f.fsReadHome('.docker/config.json') },
    // list — success (non-git tail) + every rejection branch.
    { label: 'list 200 non-git dir', run: (f) => f.fsListHome('') },
    { label: 'list 400 traversal', run: (f) => f.fsListHome('../x') },
    { label: 'list 404 target is a file', run: (f) => f.fsListHome('note.txt') },
    { label: 'list 404 missing dir', run: (f) => f.fsListHome('nodir') },
  ];
  for (const kase of nonGitCases) await assertParity(plain.dir, kase);

  // Git fixture: exercises listAt's SECOND phase — the ignoredPaths git
  // check-ignore await. In the Effect core this is the Effect.promise(runIgnore)
  // tail split off the sync prefix; in the legacy body it is one straight-line
  // try. Both must annotate `.ignored` entries identically (D6: the await stays
  // inside the in-core try/catch, so a git failure would be a 404, not a 500).
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-core-parity' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(path.join(repo.worktree, 'fixture.ignored'), 'not searchable\n');
  writeFileSync(path.join(repo.worktree, 'tracked.txt'), 'tracked\n');

  await assertParity(repo.worktree, {
    label: 'list 200 git two-phase (runIgnore tail)',
    run: (f) => f.fsListHome(''),
  });

  // Sanity: the git list really did travel the annotated (git:true) tail, so the
  // parity above is meaningful and not a degenerate git:false result.
  const gitListed = (await makeFiles(repo.worktree, true).fsListHome('')) as {
    status: number;
    body: { git?: unknown; entries?: unknown };
  };
  assert.equal(gitListed.status, 200);
  assert.equal(gitListed.body.git, true, 'git fixture must take the annotated list tail');
  const entries = gitListed.body.entries;
  assert.ok(Array.isArray(entries));
  assert.equal(
    entries.some(
      (e) =>
        typeof e === 'object' &&
        e !== null &&
        'name' in e &&
        e.name === 'fixture.ignored' &&
        'ignored' in e &&
        e.ignored === true,
    ),
    true,
    'the two-phase runIgnore tail must mark the gitignored entry',
  );
});

test('list D6 reject arm: ignoredPaths rejection is in-core 404 on both dispatch paths', async (t) => {
  // D6: ignoredPaths is shared; a real Promise rejection of its await (unbound
  // execFileP — the same error exec.ts publishes when no delegate is bound —
  // or a runBounded throw) is caught in-core on BOTH twins:
  //   - Legacy: listAtLegacy's single try at files.ts:~713-715 → failure(err)
  //   - Effect: runIgnore's own try/catch at files.ts:~797-798 → failure(err)
  //     (resolved Promise, so Effect.promise does NOT die)
  // If a later edit drops only runIgnore's catch, Effect would Effect.promise-die
  // → settler 500 `{ok:false, reason:'internal'}` + console.error, while legacy
  // stays 404 — and every other case in this file would still pass.
  //
  // git classification is deriveRepo via execFileSync, not execFileP, so the
  // spy below still takes the git:true tail; the await that rejects is check-ignore.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-core-parity-d6' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, '.gitignore'), '*.ignored\n');
  writeFileSync(path.join(repo.worktree, 'fixture.ignored'), 'not searchable\n');
  writeFileSync(path.join(repo.worktree, 'tracked.txt'), 'tracked\n');

  const spy = spyOn(exec, 'execFileP').mockImplementation(
    (): Promise<never> => Promise.reject(new Error('execFileP process runtime is not bound')),
  );
  t.after(() => spy.mockRestore());

  const expected = {
    via: 'then' as const,
    status: 404,
    body: { ok: false, reason: 'not found' },
  };

  for (const withRunner of [true, false] as const) {
    const label = withRunner ? 'Effect' : 'legacy';
    const captured = await withConsoleCapture(() =>
      settleFsOp(makeFiles(repo.worktree, withRunner).fsListHome('')),
    );
    assert.deepEqual(
      captured.result,
      expected,
      `${label}: ignoredPaths reject must settle via then as in-core 404, not catch/500`,
    );
    assert.deepEqual(captured.logs, [], `${label}: D6 reject arm must not log`);
  }

  spy.mockRestore();
  // Same git tree lists 200 git:true once execFileP is bound again — the 404 was
  // the reject arm, not a degenerate git:false fixture.
  const restored = (await makeFiles(repo.worktree, true).fsListHome('')) as {
    status: number;
    body: { git?: unknown };
  };
  assert.equal(restored.status, 200);
  assert.equal(restored.body.git, true, 'git fixture must take the annotated list tail');
});

test('injected runControlDetached is invoked on the Effect factory and skipped when absent', async (t) => {
  // Equality of the two factory wires cannot see a dead dispatcher that always
  // took *Legacy (legacy is the characterization). Wrapping the injected runner
  // locks the seam: one read + one list must fan out n=2 on the Effect factory
  // and leave the counter unchanged on the runner-absent factory.
  const plain = makePlainDir();
  t.after(() => plain.cleanup());
  writeFileSync(path.join(plain.dir, 'note.txt'), 'hello\nworld\n');
  mkdirSync(path.join(plain.dir, 'sub'));

  let n = 0;
  const wrapping = <A>(effect: Effect.Effect<A, never, never>): Promise<A> => {
    n += 1;
    return runControlDetached(effect);
  };

  const filesEffect = makeFiles(plain.dir, true, wrapping);
  await filesEffect.fsReadHome('note.txt');
  await filesEffect.fsListHome('');
  assert.equal(
    n,
    2,
    'one read + one list on the Effect factory must discharge through the injected runner',
  );

  const afterEffect = n;
  const filesLegacy = makeFiles(plain.dir, false, wrapping);
  await filesLegacy.fsReadHome('note.txt');
  await filesLegacy.fsListHome('');
  assert.equal(
    n,
    afterEffect,
    'runner-absent factory must not invoke the injected runner (n unchanged, expected 0 more)',
  );
  assert.equal(afterEffect, 2);
});

test('runControlDetached surfaces a died Effect as the RAW defect (§6 Q4 seam identity)', async () => {
  // §6 Q4 (BINDING): runPromiseWith rejects with causeSquash(exit.cause) — the
  // raw defect, identity-preserved, with NO FiberFailure wrapper. Effect.promise
  // turns a rejected promise into a DEFECT (die), which is exactly how an
  // uncaught async tail (search's git-ignore await, P9.5) would surface. This
  // pins the mechanism so slice 3's transport story rests on observed behavior.
  const sentinel = new Error('files-core-parity Q4 sentinel');
  let caught: unknown;
  try {
    await runControlDetached(Effect.promise<never>(() => Promise.reject(sentinel)));
    assert.fail('a died Effect must reject the discharged promise');
  } catch (err) {
    caught = err;
  }
  assert.equal(caught, sentinel, 'the rejection must be the RAW defect by identity (no wrapper)');
  assert.ok(caught instanceof Error);
  assert.equal((caught as Error).message, 'files-core-parity Q4 sentinel');
});
