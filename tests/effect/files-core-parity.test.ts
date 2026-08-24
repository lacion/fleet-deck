// tests/effect/files-core-parity.test.ts — P9.3 (slices 1+2+3) rejection-parity pin.
//
// P9.3 moves the files read core (readAt), list core (listAt, incl. the
// git-ignore tail), and search core (searchAt, 2-in-flight cap + backend
// tail) behind a per-op Effect dispatcher discharged by the
// ctx-resident runControlDetached. This pin fixes the ONE property the whole
// slice hangs on: the Effect core is a byte-for-byte substitute for the legacy
// async body across BOTH success and every rejection wire — with no divergence
// in console output ("log dialect").
//
// The dispatcher chooses the Effect core iff `EFFECT_CORE_FILES && runFs`
// (files.ts §dispatchList/§dispatchRead/§dispatchSearch). So the two arms
// below are the exact two branches the flag toggles:
//   - filesEffect: a real runControlDetached is injected  -> Effect core runs
//                  (this IS the EFFECT_CORE_FILES = true production path).
//   - filesLegacy: NO runner injected                     -> *Legacy body runs
//                  (byte-identical to the EFFECT_CORE_FILES = false rollback,
//                   and to every standalone-factory caller e.g.
//                   files-run-bounded's filesAt()).
// Asserting the two arms agree, wire-for-wire and log-for-log, pins the flag's
// two positions as observationally identical — the rollback twin is safe.
//
// Follow-up pins lock the seams equality cannot see:
//   - D6 reject arm (list): a git tree whose ignoredPaths await REJECTS must
//     still settle via then as in-core 404 `{ok:false, reason:'not found'}` on
//     BOTH dispatch paths (runIgnore's catch vs listAtLegacy's single try),
//     never a transport 500. Dropping only the Effect catch would green every
//     other case.
//   - Dispatcher liveness: wrapping the injected runner must observe n=3 for
//     one read + one list + one search on the Effect factory and n unchanged
//     without a runner, so a ternary that always took *Legacy cannot green
//     this file.
// The last case pins the §6 Q4 ruling directly at the seam: a died Effect
// discharged through the production runControlDetached rejects with the RAW
// defect (identity-preserved), never a FiberFailure wrapper. That is the exact
// mechanism by which search's uncaught backend tail surfaces a transport 500
// (now pinned on both dispatch paths) — and, by contrast, why list's IN-CORE
// try/catch (D6) keeps its rejection a 404.
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

// Search parity: like assertParity but tolerant of `elapsed_ms`, which is
// wall-clock and legitimately differs between the two sequential runs. On a 200
// wire it is asserted present+numeric on both arms, then stripped before the
// byte compare; every other (guard/resolve-error) wire is compared verbatim.
async function assertSearchParity(root: string, kase: ParityCase): Promise<void> {
  const strip = (r: unknown): unknown => {
    if (r && typeof r === 'object' && 'body' in r) {
      const body = (r as { body: unknown }).body;
      if (body && typeof body === 'object' && 'elapsed_ms' in body) {
        assert.equal(
          typeof (body as { elapsed_ms: unknown }).elapsed_ms,
          'number',
          `${kase.label}: a 200 search wire must carry a numeric elapsed_ms`,
        );
        const { elapsed_ms: _elapsed, ...rest } = body as Record<string, unknown>;
        return { ...(r as Record<string, unknown>), body: rest };
      }
    }
    return r;
  };
  const eff = await withConsoleCapture(() => kase.run(makeFiles(root, true)));
  const leg = await withConsoleCapture(() => kase.run(makeFiles(root, false)));
  assert.deepEqual(
    strip(eff.result),
    strip(leg.result),
    `${kase.label}: Effect-core search wire must equal the legacy wire (mod elapsed_ms)`,
  );
  assert.deepEqual(eff.logs, leg.logs, `${kase.label}: console transcripts must match`);
  assert.deepEqual(eff.logs, [], `${kase.label}: a settled search must not log`);
}

// ── P9.3 Slice 3 · searchesInFlight 2-in-flight admission cap (D3) ────────────
// The cap is a single module-level `let searchesInFlight` (files.ts:129) shared
// by EVERY search caller (session + home). searchAt must admit at most two
// concurrent searches and refuse the third with an EXACT 429 wire, then release
// the slot on the operation's try/finally — on success AND on a backend
// rejection (the finally has NO catch: leg-C propagates raw, D6-asymmetry).
//
// D3 (the slice-3 precondition, §6): admission must decide SYNCHRONOUSLY. On the
// legacy path the plain-async prefix increments before its first await; on the
// Effect path the increment lives in the Effect.sync admission prefix, which
// runControlDetached runs EAGERLY (to the first Effect.promise suspension) before
// returning — so two un-awaited calls both reserve before a third reads the
// counter. This helper pins the SAME observable cap on either arm: it is run
// against the LEGACY path first (characterization, against current code) and the
// EFFECT path after the conversion, proving the increment did not slip past the
// async boundary and that the slot is released on every exit.
const SEARCH_CAP_REFUSAL = {
  status: 429,
  body: { ok: false, reason: 'search busy — try again' },
} as const;

async function assertSearchCap(
  t: { after: (fn: () => void) => void },
  withRunner: boolean,
): Promise<void> {
  // Admission / refusal / release-on-success over a walk fixture large enough
  // that the first two searches are genuinely still in flight (walkSearch yields
  // across 520 files) while the third reads the counter.
  const plain = makePlainDir();
  t.after(() => plain.cleanup());
  for (let i = 0; i < 520; i += 1) {
    writeFileSync(path.join(plain.dir, `file-${String(i).padStart(4, '0')}.txt`), '');
  }
  const files = makeFiles(plain.dir, withRunner);

  const first = files.fsSearchHome('zz', { mode: 'name' });
  const second = files.fsSearchHome('zz', { mode: 'name' });
  const third = await files.fsSearchHome('zz', { mode: 'name' });
  assert.deepEqual(
    third,
    SEARCH_CAP_REFUSAL,
    'the third concurrent search must be refused with the exact 429 wire',
  );
  const admitted = await Promise.all([first, second]);
  assert.deepEqual(
    admitted.map((r) => (r as { status: number }).status),
    [200, 200],
    'the first two concurrent searches are admitted',
  );
  const afterRelease = await files.fsSearchHome('zz', { mode: 'name' });
  assert.equal(
    (afterRelease as { status: number }).status,
    200,
    'settled searches release both admission slots (finally on the success path)',
  );
  // D3: a 429 that incremented (and never decremented) would leave the counter
  // at 1 after the in-flight pair completes — the single afterRelease 200
  // would still pass. Filling the cap again with two concurrent searches
  // asserts BOTH are 200: a leaked +1 would admit one and 429 the other.
  const refillA = files.fsSearchHome('zz', { mode: 'name' });
  const refillB = files.fsSearchHome('zz', { mode: 'name' });
  const refilled = await Promise.all([refillA, refillB]);
  assert.deepEqual(
    refilled.map((r) => (r as { status: number }).status),
    [200, 200],
    'two concurrent post-release searches both admit — a 429 that incremented would leak a slot and refuse one',
  );

  // Release-on-FAILURE (the try/finally, NO catch): a git root whose backend
  // await REJECTS (execFileP unbound) must still decrement. Three SEQUENTIAL
  // failing searches must each REJECT — never resolve a leaked 429. If the finally
  // were dropped the counter would climb 0→1→2 and the third would settle 429.
  // git classification is deriveRepo via execFileSync, so the git:true tail is
  // still taken; only the check-ignore/grep await (execFileP) rejects.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-cap-failure' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, 'tracked.txt'), 'needle here\n');
  const failFiles = makeFiles(repo.worktree, withRunner);
  const spy = spyOn(exec, 'execFileP').mockImplementation(
    (): Promise<never> => Promise.reject(new Error('execFileP process runtime is not bound')),
  );
  try {
    for (let i = 0; i < 3; i += 1) {
      await assert.rejects(
        failFiles.fsSearchHome('needle', { mode: 'content' }),
        /execFileP process runtime is not bound/,
        `failing search #${String(i)} must reject (slot released via finally, never a leaked 429)`,
      );
    }
  } finally {
    spy.mockRestore();
  }
  // Counter is back to 0: a normal search on the walk root is admitted again.
  const recovered = makeFiles(plain.dir, withRunner);
  const ok = await recovered.fsSearchHome('zz', { mode: 'name' });
  assert.equal(
    (ok as { status: number }).status,
    200,
    'three failed searches each released their slot (finally on the reject path)',
  );
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
  // locks the seam: one read + one list + one search must fan out n=3 on the
  // Effect factory and leave the counter unchanged on the runner-absent factory.
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
  await filesEffect.fsSearchHome('note', { mode: 'name' });
  assert.equal(
    n,
    3,
    'one read + one list + one search on the Effect factory must each discharge through the injected runner',
  );

  const afterEffect = n;
  const filesLegacy = makeFiles(plain.dir, false, wrapping);
  await filesLegacy.fsReadHome('note.txt');
  await filesLegacy.fsListHome('');
  await filesLegacy.fsSearchHome('note', { mode: 'name' });
  assert.equal(
    n,
    afterEffect,
    'runner-absent factory must not invoke the injected runner (n unchanged, expected 0 more)',
  );
  assert.equal(afterEffect, 3);
});

test('runControlDetached surfaces a died Effect as the RAW defect (§6 Q4 seam identity)', async () => {
  // §6 Q4 (BINDING): runPromiseWith rejects with causeSquash(exit.cause) — the
  // raw defect, identity-preserved, with NO FiberFailure wrapper. Effect.promise
  // turns a rejected promise into a DEFECT (die), which is exactly how search's
  // uncaught backend await surfaces a transport 500 (now pinned on both dispatch
  // paths). This pins the seam identity independently of the search D6 test.
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

test('legacy searchAt honors the 2-in-flight cap: admits two, refuses the third, releases on success and failure', async (t) => {
  // D3 characterization (slice-3 precondition): the LEGACY search body reserves a
  // slot before its first await and frees it in a finally on BOTH the success and
  // the reject exit. Pinned against the current code FIRST; the Effect-core arm
  // (below) is added only after the conversion so this cannot false-green.
  await assertSearchCap(t, false);
});

test('files search Effect core matches the legacy body across success, guard, and resolve-error wires', async (t) => {
  const plain = makePlainDir();
  t.after(() => plain.cleanup());
  writeFileSync(path.join(plain.dir, 'note.txt'), 'hello\nworld\n');
  mkdirSync(path.join(plain.dir, 'sub'));

  // Walk backend + every synchronous guard branch (400s). Both arms byte-identical
  // (mod elapsed_ms), no logging on the settled path.
  const walkCases: ParityCase[] = [
    { label: 'search 200 walk name hit', run: (f) => f.fsSearchHome('note', { mode: 'name' }) },
    { label: 'search 200 walk name miss', run: (f) => f.fsSearchHome('zzzz', { mode: 'name' }) },
    {
      label: 'search 200 walk content hit',
      run: (f) => f.fsSearchHome('hello', { mode: 'content' }),
    },
    { label: 'search 400 q too short', run: (f) => f.fsSearchHome('x', { mode: 'name' }) },
    {
      label: 'search 400 q too long',
      run: (f) => f.fsSearchHome('a'.repeat(257), { mode: 'name' }),
    },
    { label: 'search 400 invalid mode', run: (f) => f.fsSearchHome('valid', { mode: 'bogus' }) },
    { label: 'search 400 mode missing', run: (f) => f.fsSearchHome('valid') },
  ];
  for (const kase of walkCases) await assertSearchParity(plain.dir, kase);

  // Resolve-error (410): a browse root that does not exist. The guards pass, so
  // the resolve() error wire (done:true prefix branch) is what both arms return.
  const gone = path.join(plain.dir, 'no-such-root');
  await assertSearchParity(gone, {
    label: 'search 410 root gone',
    run: (f) => f.fsSearchHome('valid', { mode: 'name' }),
  });

  // Git backend end-to-end (gitSearch): both arms must agree on hits/backend/
  // truncated (mod elapsed_ms). tracked.txt is untracked → --others/--untracked.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-search-parity' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, 'tracked.txt'), 'needle in tracked\n');
  const gitCases: ParityCase[] = [
    {
      label: 'search 200 git content hit',
      run: (f) => f.fsSearchHome('needle', { mode: 'content' }),
    },
    { label: 'search 200 git name hit', run: (f) => f.fsSearchHome('tracked', { mode: 'name' }) },
    {
      label: 'search 200 git content miss',
      run: (f) => f.fsSearchHome('definitely-no-such-needle', { mode: 'content' }),
    },
  ];
  for (const kase of gitCases) await assertSearchParity(repo.worktree, kase);

  // Sanity: the git searches really traveled the git backend, so parity above is
  // meaningful and not a degenerate walk result.
  const gitSearched = (await makeFiles(repo.worktree, true).fsSearchHome('needle', {
    mode: 'content',
  })) as { status: number; body: { backend?: unknown; hits?: unknown } };
  assert.equal(gitSearched.status, 200);
  assert.equal(gitSearched.body.backend, 'git', 'git fixture must take the git search backend');
});

test('search no-catch reject arm: a backend rejection is transport 500 on both dispatch paths (D6 asymmetry)', async (t) => {
  // The deliberate contrast to list's D6 404 fold: searchAt's backend await sits
  // in a try/FINALLY with NO catch. A real rejection (unbound execFileP) therefore
  // PROPAGATES —
  //   - Legacy: searchAtLegacy's finally releases the slot, then the promise rejects.
  //   - Effect: runSearch's finally releases, then the promise rejects → Effect.promise
  //     dies → runControlDetached rejects with the RAW cause (§6 Q4).
  // Both surface at the transport settler as via:'catch' + 500 {ok:false,reason:'internal'}
  // + an identical console.error line. If a later edit added a catch→failure to
  // runSearch (matching list's runIgnore), this would flip to via:'then' 404 here.
  //
  // git classification is deriveRepo via execFileSync, so the git:true backend is
  // still selected; only gitSearch's grep await (execFileP) rejects.
  const repo = makeRepoWithWorktree({ repoName: 'fleetdeck-files-search-d6' });
  t.after(() => repo.cleanup());
  writeFileSync(path.join(repo.worktree, 'tracked.txt'), 'needle here\n');

  const spy = spyOn(exec, 'execFileP').mockImplementation(
    (): Promise<never> => Promise.reject(new Error('execFileP process runtime is not bound')),
  );
  t.after(() => spy.mockRestore());

  const arms: Record<'Effect' | 'legacy', { result: unknown; logs: string[] }> = {
    Effect: await withConsoleCapture(() =>
      settleFsOp(makeFiles(repo.worktree, true).fsSearchHome('needle', { mode: 'content' })),
    ),
    legacy: await withConsoleCapture(() =>
      settleFsOp(makeFiles(repo.worktree, false).fsSearchHome('needle', { mode: 'content' })),
    ),
  };

  const expected = { via: 'catch' as const, status: 500, body: { ok: false, reason: 'internal' } };
  assert.deepEqual(
    arms.Effect.result,
    expected,
    'Effect: no-catch backend reject must settle via catch 500',
  );
  assert.deepEqual(
    arms.legacy.result,
    expected,
    'legacy: no-catch backend reject must settle via catch 500',
  );
  assert.deepEqual(
    arms.Effect.logs,
    arms.legacy.logs,
    'the console.error transcripts (settler) must match byte-for-byte across arms',
  );
  assert.equal(
    arms.Effect.logs.length,
    1,
    'exactly one console.error (the transport settler) fires on the Effect arm',
  );
});

test('Effect searchAtEffect honors the 2-in-flight cap synchronously (D3): admits two, refuses the third, releases on success and failure', async (t) => {
  // The mandatory D3-GAP pin: the SAME cap, now on the Effect core. The reservation
  // lives in the Effect.sync admission prefix, which runControlDetached runs eagerly
  // before the Effect.promise backend suspension — so admission still decides
  // synchronously and the slot is released on both the success return and the
  // propagated (no-catch) rejection.
  await assertSearchCap(t, true);
});
