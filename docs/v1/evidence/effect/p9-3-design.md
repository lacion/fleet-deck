# P9.3 Design — `src/daemon/files.ts` read surfaces (session-FS + home-FS)

> **Stamped into the repo 2026-08-24.** Copied verbatim from the adjudicated draft `/tmp/fd-effect/p9-3-design-draft.md`; §6 carries the orchestrator's rulings. Slices 1+2 already implemented + reviewed on worktree branch `tmp/p9-3-design` @ `4aafe9c2`, awaiting integration; slice 3 (search core) remains.

> Effect v4 migration. This is a DESIGN/INVENTORY document. No source changed.
> Follows the section skeleton of `docs/v1/evidence/effect/p9-1-design.md`.
> Scope (from `p9-completion-map.md`, files.ts section): the two read-only FS
> route families and their cores —
> `GET /api/sessions/:sid/fs/{list,read,search}` (http.ts:2209-2224) and
> `GET /api/fs/{list,read,search}` (http.ts:2226-2239) — dispatching into the six
> `createFiles` entry points `fsList/fsRead/fsSearch(sid,…)` +
> `fsListHome/fsReadHome/fsSearchHome(…)` (files.ts:812-820).
>
> **Frozen this package.** The bounded git child-process helper `runBounded`
> (files.ts:352) STAYS a named adapter (completion-map ruling; do NOT design its
> conversion). NB this is a DIFFERENT symbol from the `ProcessRunner.runBounded`
> capability in exec.ts:82 / process-runner.ts:51 — the files.ts one is a local
> `execFileP` wrapper reached only by legs B and D below.
>
> **Baseline (verified read-only, this session).**
> `tests/files-run-bounded.test.ts` → 6 pass. `tests/session-fs.test.ts` → 17
> pass. 23 characterization tests green at HEAD.

---

## §1 INVENTORY

### 1.0 Caller map (blast radius)

The six cores are **HTTP-only**. The sole runtime callers are the two GET
dispatch blocks in `http.ts` (session 2215-2223, home 2229-2237); the only other
references are the destructure/re-export in `derive.ts:1261` and `:1399-1404`.
No non-HTTP domain caller, no scheduler, no adopt/spawn path touches them.
`fleetd.bundle.mjs` contains minified copies — it is a BUILD ARTIFACT regenerated
from `src/`, never hand-edited, and is not part of any slice's diff.

Test callers: `session-fs.test.ts` drives the routes end-to-end through the real
daemon (`startDaemon()` → HTTP → `settleFilesystemOperation`), plus two direct
`createFiles(fakeCtx)` unit calls (`:481`, and the `filesAt` helper).
`files-run-bounded.test.ts` calls `createFiles` directly via its `filesAt` helper
(files-run-bounded.test.ts:61-70) and imports `runBounded` directly.

Both direct-`createFiles` helpers build ctx as `{ q, browseRootChoice }` with **no
`runControlDetached`** — see §3 for why that matters (they will exercise the
legacy fallback, so the Effect path needs its own pin).

### 1.1 Async legs (lettered)

Every `await` / async surface in files.ts, with what awaits what and the
bordering sync work:

- **Leg A — `listAt` git-ignore tail** — `createFiles.listAt` (files.ts:623-690),
  the single `await` at ~679-685: `await ignoredPaths(root, rels, SEARCH_TIMEOUT_MS)`.
  Reached ONLY when the resolved root is a git tree (`git === true`). Bordering
  sync: the ENTIRE entries build (validate → resolve → `safeJoin` → `realpathInside`
  → `readdirSync` → per-entry `lstatSync` window → dirs-first sort → splice to
  `LIST_MAX`) is synchronous; the tail only annotates `entry.ignored` from the
  check-ignore result. **The await is INSIDE `listAt`'s `try/catch`** → a rejection
  maps to `failure(err)` → 404 `{ok:false,reason:'not found'}` (err is not a
  `PathError`), NOT the transport 500. (Byte-freeze D6.)

- **Leg B — `ignoredPaths`** — files.ts:369-383. `await runBounded('git',
  ['check-ignore','-z','--stdin'], {cwd:root, timeoutMs, maxBytes, input:<NUL-joined
  rels>})`. One git child process. Called only by Leg A. Pinned end-to-end by
  files-run-bounded.test.ts:197 ("check-ignore consumes the NUL-delimited stdin").

- **Leg C — `searchAt` backend await** — `createFiles.searchAt` (files.ts:765-803).
  `await (git ? gitSearch(…) : walkSearch(…))` inside a `try { … } finally {
  searchesInFlight -= 1 }` (dec at :801). Bordering sync (all on the admitting
  turn): q-length 400 → mode 400 → `resolve()` (404/410) → `searchesInFlight >= 2`
  → 429 (:778) → `searchesInFlight += 1` (:781) → capture `started`/deadline.
  **The `try` has a `finally` but NO `catch`** → a backend throw propagates out of
  `searchAt` as a rejected Promise → transport `settleFilesystemOperation.catch` →
  500 `{ok:false,reason:'internal'}`. (Asymmetry vs Leg A — byte-freeze D6.)

- **Leg D — `gitSearch`** — files.ts:428-487. Name mode: `await runBounded('git',
  ['ls-files',…])`. Content mode: `await runBounded('git', ['grep',…])`, plus an
  optional SECOND `await runBounded(...)` retry when the first returns a git option
  error (the color.ui/color.grep-neutralized fallback, BUG-027). 1-2 git child
  processes. Called only by Leg C when `git === true`. Backend label `'git'`.

- **Leg E — `walkSearch`** — files.ts:502-617. Non-git DFS: synchronous fs
  throughout (`readdirSync`, `lstatSync`, bounded open-file read), with a
  cooperative `await yieldToLoop()` every `WALK_YIELD_EVERY = 512` visited entries
  (:550). NO child process. Called only by Leg C when `git === false`. Backend
  label `'walk'`. The header comment (files.ts:489-495) states the async-on-purpose
  rationale: keep the 2-in-flight cap meaningful by yielding the loop.

- **Leg F — `readAt`** — `createFiles.readAt` (files.ts:698-763). **NO `await`** —
  `async` + `// eslint-disable-next-line @typescript-eslint/require-await` (:695-697)
  purely to satisfy the uniform `.then` transport contract. All work (validate →
  resolve → `safeJoin` → `realpathInside` → `lstatSync` file/dir/size checks →
  bounded `readFileSync` → null-byte `isBinary` sniff → newline-clamped truncation)
  is synchronous. → collapses to a pure `Effect.sync` core (no async tail).

- **Leg G — `runBounded`** — files.ts:352-363. FROZEN named adapter wrapping
  `execFileP` with `{timeoutMs, maxBytes, cwd, input}`, immediate-SIGKILL-on-timeout,
  shared exact byte cap across stdout+stderr, settle-after-reap. Reached by legs B
  and D. **Out of scope** — do not convert. Its bounding/kill contract is pinned by
  files-run-bounded.test.ts:72/110/132/164.

- **Leg H — `yieldToLoop`** — files.ts:497-500, `() => new Promise(resolve =>
  setImmediate(resolve))`. The cooperative loop-yield inside Leg E. Part of
  walkSearch's coarse operation; not a child process, not an independent leg to
  convert.

**Sync leaves that STAY sync** (the "2/2 ctx.q" the completion map counts):
`resolveRoot(ctx, sid)` (files.ts:271-283) — `ctx.q.getSession.get(sid)` (:272)
and `ctx.q.spawnBySession.get(sid)` (:274), plus `statSync`/`realpathSync` on the
candidate working tree; returns 404 unknown-session / 410 tree-gone.
`resolveBrowseRoot(ctx)` (files.ts:294-322) — reads `ctx.browseRootChoice()`, does
sync fs, 410s a deleted/gone root or a filesystem-root (`/`) choice. Both are
called from the SYNC prefix of every core and never move behind an await.

### 1.2 Wire dialect per route

The transport is `settleFilesystemOperation(res, scope, operation)`
(http.ts:834-847): `operation.then(({status,body}) => json(res,status,body))
.catch(err => { console.error(\`fleetd ${scope} filesystem error:\`, err);
json(res,500,{ok:false,reason:'internal'}) })`. Every core resolves its OWN
`{status, body}`; the transport 500 arm fires only on a Promise REJECTION (Leg C
only — see D6). `scope` is `'session'` | `'home'`. All six cores share the SAME
impl per op (a `RootResolver` thunk selects session vs home root), so the wire is
identical between `fsX` and `fsXHome` except the root-resolution family.

**Shared root-resolution outcomes** (from the resolver thunk, emitted before any
leg-A/C work):
- session `sessionRoot(sid)` → 404 `{ok:false,reason:'unknown session'}` (no row);
  410 `{ok:false,reason:'working tree no longer exists'}` (candidate missing / not
  a dir / realpath throws).
- home `homeRoot` → 410 `{ok:false,reason:<browseRootGoneReason(...)>}` (resolved
  path missing); 410 `{ok:false,reason:\`<sourceName> must not be the filesystem
  root\`}` (choice is `/`).

**`/…/fs/list` (Leg A)** — `listAt`:
- 400 `{ok:false,reason:'invalid path'}` — `validateRelPath` (PathError 400).
- 404 `{ok:false,reason:'not found'}` — `.git`/credential segment (PathError 404),
  `realpathInside` denial, non-dir / symlinked target, or generic catch fallback.
- root-resolution 404/410 (above).
- 200 `{ok:true, path, git, entries, truncated}`; `entries[]` =
  `{name,type,size,mtime,ignored}`, dirs-first then name, `truncated =
  names.length > LIST_MAX`. `ignored` filled by Leg A only when `git`; else `false`.

**`/…/fs/read` (Leg F)** — `readAt`:
- 400 `{ok:false,reason:'invalid path'}`.
- 404 `{ok:false,reason:'not found'}` — credential/.git/realpath/symlink/not-a-file
  (incl. FIFO / non-regular → refused promptly).
- 404 `{ok:false,reason:'is a directory'}` — abs===root or `lstat` isDirectory.
- root-resolution 404/410.
- 200 `{ok:true, path, size, mtime, binary, truncated, content?}`; `content` OMITTED
  when `binary`; `truncated` clips to the last newline within `READ_MAX` (or empty).

**`/…/fs/search` (Legs C/D/E)** — `searchAt`, in this exact order:
- 400 `{ok:false,reason:'query must be 2–256 characters'}` — q length.
- 400 `{ok:false,reason:'invalid search mode'}` — mode ∉ {content,name}.
- root-resolution 404/410 — **runs BEFORE the 429** (unknown session + busy → 404,
  not 429; ordering is byte-frozen, D2).
- 429 `{ok:false,reason:'search busy — try again'}` — `searchesInFlight >= 2`.
- 200 `{ok:true, mode, q, backend:'git'|'walk', hits, truncated, elapsed_ms}`.
- 500 `{ok:false,reason:'internal'}` — backend throw escaping the finally (transport
  `.catch`; the ONLY route with a reachable transport-500, D6).

---

## §2 TARGET SHAPES

### 2.1 The decision: convert cores, keep the transport — do NOT move to runRequest

The GET FS routes are **not on the P6.4 transport today** — they use the bespoke
`settleFilesystemOperation` (a raw `.then/.catch` join on the core Promise), NOT
`runRequest` / `mapEffectRouteExit` / a settler. The minimal, precedent-matching
P9.3 is therefore identical in shape to how P9.1 handled the already-async control
routes (retention `dismiss`, spawns):

> **Convert the three cores to Effects; discharge them to native Promises through
> the INJECTED `runControlDetached`, inside the six dispatchers, behind an
> `EFFECT_CORE_FILES` flag. Leave `settleFilesystemOperation` byte-for-byte
> untouched.**

The transport still receives `Promise<{status, body}>` and still does
`.then(json)/.catch(500)`. This adds **no `run*` call site** (`runControlDetached`
is the injected capability, not `Effect.run*`), **no new settler**, and **no
`runRequest` wiring**. It matches the completion map's own **P9.3 / P9.5** split:
P9.3 removes the async from the cores; the transport move onto `runRequest` (rows
G5/G6 in the leftover-HTTP table) is **P9.5** and out of scope here.

This is also correct on the merits: these are READS. There is no write to protect
on shutdown, so the deliberately-UNSUPERVISED `runControlDetached` (ingress-
supervisor.ts:64-71, `Effect.runPromiseWith(Context.empty())`) is the right runner
and needs no start-once recorder / shutdown-join (the D7 machinery the async
CONTROL routes need). `res.done` already gates `closeClients` exactly as it gated
the legacy `.then(json)` chain.

### 2.2 Runner threading (answering the "how does the core reach the runtime" question)

`runControlDetached` is ALREADY a field on the derive `ctx` object
(derive.ts:1091-1124; wired `program.ts:794` ← `ingress.runControlDetached`), and
`createFiles(ctx)` (derive.ts:1260) receives that same ctx. **The only wiring
change P9.3 needs is widening the `FilesCtx` interface** (files.ts:29-32) with:

```ts
type RunControlDetached = <A>(effect: Effect.Effect<A, never, never>) => Promise<A>;
interface FilesCtx {
  q: /* … unchanged … */;
  browseRootChoice: () => BrowseRootChoice;
  // P9.3: the ingress-owned unsupervised runner that discharges the FS read
  // cores. Optional — absent in standalone factory tests → legacy fallback.
  runControlDetached?: RunControlDetached;
}
```

`RunControlDetached` is declared LOCALLY (mirroring ingress-supervisor.ts:71 and
retention.ts:95) to avoid a new `files.ts → retention.ts` import edge — it is one
line and structurally frozen. No `program.ts`/`derive.ts` edit is required.

### 2.3 Per-core Effect shapes

All three follow the canonical two-phase pattern from `dismissSessionEffect`
(retention.ts) / `controlAsyncWorkflow` (control.ts:96-111): a synchronous
`Effect.sync` prefix that either resolves a TERMINAL wire or CONSTRUCTS (but does
not run) a coarse async thunk, then a `flatMap` that discharges it.

**`readAtEffect` (Leg F) — pure sync, no tail:**
```ts
const readAtEffect = (resolve: RootResolver, relPath: string) =>
  Effect.sync((): FsResult => readAtBody(resolve, relPath)); // the current readAt body, verbatim
```
No `flatMap`, no `Effect.promise` — readAt has no real await. Discharged via
`runControlDetached` it resolves on a microtask, exactly as the `async` legacy body
does today.

**`listAtEffect` (Leg A) — sync prefix + optional git-ignore tail:**
```ts
type ListStep =
  | { done: true;  wire: FsResult }
  | { done: false; runIgnore: () => Promise<FsResult> };

const listAtEffect = (resolve, relPath) =>
  Effect.sync((): ListStep => {
    // validate → resolve → safeJoin → realpathInside → readdir → lstat window →
    // sort → splice(LIST_MAX). All the current sync body.
    // NON-git or any thrown guard → { done: true, wire } (200 with ignored:false,
    //   or failure(err) mapped exactly as the current try/catch does).
    // git → { done: false, runIgnore } where runIgnore awaits Leg B and annotates.
  }).pipe(Effect.flatMap(step =>
    step.done ? Effect.succeed(step.wire) : Effect.promise(step.runIgnore)));
```
`runIgnore` keeps the git-ignore await inside a `try/catch → failure(err)` so a
check-ignore rejection maps to 404, byte-identical to the legacy single
`try/catch` (D6). Every guard `throw` in the sync prefix is caught in the prefix and
returned as `{done:true, wire: failure(err)}` — the prefix NEVER dies.

**`searchAtEffect` (Legs C/D/E) — sync admission prefix + backend tail:**
```ts
type SearchStep =
  | { done: true;  wire: FsResult }              // 400 / 404 / 410 / 429
  | { done: false; runSearch: () => Promise<FsResult> };

const searchAtEffect = (resolve, q, opts) =>
  Effect.sync((): SearchStep => {
    // q-length 400 → mode 400 → resolve() 404/410 → (searchesInFlight >= 2 → 429)
    // → searchesInFlight += 1 → capture started/deadline.
    // Any terminal → { done: true, wire }.
    // Admitted → { done: false, runSearch } where runSearch =
    //   async () => { try { const r = git ? await gitSearch(...) : await walkSearch(...);
    //                       return okWire(r) } finally { searchesInFlight -= 1 } }
  }).pipe(Effect.flatMap(step =>
    step.done ? Effect.succeed(step.wire) : Effect.promise(step.runSearch)));
```
- The `searchesInFlight += 1` MUST live in the `Effect.sync` PREFIX so admission is
  synchronous on the calling turn (D3). `runControlDetached` = `runPromiseWith`
  executes the sync prefix + flatMap EAGERLY, up to the first `Effect.promise`
  suspension — the SAME guarantee retention's dismiss atomic-DB prefix already
  relies on for "the write happens on the admitting turn". So two un-awaited
  `fsSearchHome()` calls each increment before a third's prefix reads the counter.
- `runSearch` keeps the `try { … } finally { dec }` with **no catch** (Leg C):
  `Effect.promise` treats a rejection as a DIE, `runControlDetached` surfaces it as
  a rejected Promise, and `settleFilesystemOperation.catch` emits the same 500
  `{ok:false,reason:'internal'}`. Body byte-identical; log object differs (D5).

### 2.4 Dispatcher shape (the seam) — `createFiles` return

Each of the six entry points becomes a per-op dispatch, thunk-shared session/home:
```ts
const runFs = ctx.runControlDetached;
const dispatchRead = (resolve, rel) =>
  EFFECT_CORE_FILES && runFs ? runFs(readAtEffect(resolve, rel)) : readAtLegacy(resolve, rel);
// …List, …Search analogously.
return {
  fsList:     (sid, p)     => dispatchList(sessionRoot(sid), p),
  fsRead:     (sid, p)     => dispatchRead(sessionRoot(sid), p),
  fsSearch:   (sid, q, o)  => dispatchSearch(sessionRoot(sid), q, o),
  fsListHome: (p)          => dispatchList(homeRoot, p),
  fsReadHome: (p)          => dispatchRead(homeRoot, p),
  fsSearchHome:(q, o)      => dispatchSearch(homeRoot, q, o),
};
```
One core conversion per op serves BOTH session and home (the `RootResolver` thunk
was already the shared seam). `xLegacy` = today's `listAt/readAt/searchAt`, retained
UNCHANGED as the rollback body. When `runControlDetached` is absent (standalone
factory tests) OR the flag is false, every op returns the exact legacy Promise.

---

## §3 SLICE PLAN

Green after every slice. Each slice converts ONE op (both its dispatchers) and
requires an independent review before the next. `EFFECT_CORE_FILES` is a single
module-level flag introduced `true` in Slice 1; each op migrates only when its
dispatcher is rewired, so the flag is the global kill-switch (rollback = flip false
→ all rewired ops revert to legacy).

**Characterization audit (done this session).** Both suites green at HEAD (6 + 17).
Coverage confirmed:
- `session-fs.test.ts` (daemon-driven, via `settleFilesystemOperation`): all 6
  entry points, legs A–E wire dialects, every path guard (traversal/symlink/
  credential denylist incl. `.docker/config.json`), all caps (LIST_MAX/READ_MAX/
  SEARCH_HITS/binary sniff, incl. BUG-114 lstat-window and BUG-115/116 truncated-
  ignored), lifecycle 404/410, browse-root re-root + `/` refusal, git-grep color
  neutralization (BUG-027). Once `EFFECT_CORE_FILES` is on, these run through the
  Effect cores (the daemon ctx carries `runControlDetached`).
- `files-run-bounded.test.ts` (direct `createFiles`, NO `runControlDetached`):
  runBounded byte-cap/truncation/immediate-SIGKILL/settle-after-reap (Leg G frozen
  contract), check-ignore NUL stdin (Leg B), git-grep-exit-1→empty (Leg D), and the
  synchronous 2-in-flight admission (Leg C counter).

**GAPS to pin first:**
1. **Effect-path 2-in-flight admission is UNPINNED.** files-run-bounded.test.ts's
   `filesAt` builds ctx without `runControlDetached`, so its 2-in-flight test
   exercises the LEGACY searchAt, and there is no daemon-driven 2-in-flight test.
   → In Slice 3, add a `filesAtEffect` helper that injects
   `runControlDetached: <A>(eff) => Effect.runPromise(eff)` (timing-equivalent to the
   real eager ingress runner) and re-asserts "admits exactly two, third → 429, both
   slots released" through the Effect core. This pins D3.
2. **Transport-500 body/log on a search-backend rejection is unpinned.** No test
   forces `gitSearch`/`walkSearch` to reject. It is hard to inject deterministically;
   the body `{ok:false,reason:'internal'}` is frozen by inspection, and the log-shape
   delta (D5) is documented rather than pinned. Optionally add a Slice-3 test that
   injects a `runControlDetached` whose effect throws in the tail and asserts the 500
   body (accepting the FiberFailure log line).

**Slice 1 — seam + read core (the pure one).**
- Widen `FilesCtx` (+`runControlDetached?`, +local `RunControlDetached`); add
  `EFFECT_CORE_FILES = true`; add `readAtEffect = Effect.sync(readAtBody)`; rename
  the current body to `readAtLegacy`; rewire `fsRead`/`fsReadHome` to `dispatchRead`.
- Blast radius: read only. list/search dispatchers untouched (still legacy).
- Verify: `session-fs.test.ts` read tests (now Effect path) + `files-run-bounded`
  (read legacy path) green. Rollback: flip flag / revert slice.

**Slice 2 — list core (git-ignore async tail, Leg A/B).**
- Add `listAtEffect` (two-phase, `runIgnore` in-catch → `failure`); `listAtLegacy`
  retained; rewire `fsList`/`fsListHome`.
- Verify: `session-fs.test.ts` git-list/ignored/BUG-114/115/116 (Effect path) +
  `files-run-bounded` check-ignore test green.

**Slice 3 — search core (2-in-flight counter + backend await, Legs C/D/E).**
- Add `searchAtEffect` (sync-prefix increment, `runSearch` try/finally decrement, no
  catch); `searchAtLegacy` retained; rewire `fsSearch`/`fsSearchHome`.
- Add the Slice-3 GAP pin(s): Effect-path 2-in-flight (mandatory), optional 500-body.
- Verify: `session-fs.test.ts` search tests (Effect path) + `files-run-bounded`
  2-in-flight (legacy) + new Effect-path 2-in-flight green.

**Slice 4 (optional) — adjudication / cleanup.** If the orchestrator prefers the flag
introduced `false`, this slice flips it `true` after all three cores land; otherwise
a no-op. No dead-code removal in P9.3 (legacy bodies are the rollback seam).

**Full regression each slice:** `session-fs.test.ts` + `files-run-bounded.test.ts`,
plus the effect suite touching the runner (`tests/effect/ingress-supervisor.test.ts`
run*-count pin — this design adds NO new `run*` call site, so that count must be
unchanged).

---

## §4 DANGER NOTES (byte-frozen)

- **D1 — path guards are byte-frozen.** `validateRelPath` (139-171: 400 invalid path
  / 404 for `.git` + `CREDENTIAL_SEGMENTS` `.ssh/.aws/.gnupg/.netrc/.kube`),
  `deniedName`/`deniedRelPath`, `safeJoin` (193-198), `realpathInside` (227-267:
  fleetHomeReal denial + symlink credential resolution + the `.docker/config.json`
  special case), `isBinary` (200-204 null-byte sniff). These live in the SYNC prefix
  and are NOT touched. Every guard `throw` is caught IN the prefix and returned as a
  terminal `failure(err)` wire — the `Effect.sync` prefix must never die on a guard.

- **D2 — sync stays sync; ordering frozen.** No `yield`/await may be introduced into
  a sync prefix (mirrors the retention "atomic DB block, no yield" rule). searchAt's
  gate order — q-400 → mode-400 → `resolve()` (404/410) → 429 → increment — is
  contractual (unknown-session-while-busy = 404, not 429). resolve()'s two `ctx.q`
  reads (files.ts:272,274) and its `statSync/realpathSync` stay synchronous.

- **D3 — 2-in-flight admission is SYNCHRONOUS.** `searchesInFlight` (module `let`,
  files.ts:121; shared across ALL search callers, session + home) MUST increment in
  the `Effect.sync` prefix so admission decides on the calling turn — the
  files-run-bounded test starts two un-awaited searches and expects the third to 429.
  This holds because `runControlDetached` = `Effect.runPromiseWith(Context.empty())`
  runs the sync prefix + flatMap eagerly before the first `Effect.promise` suspension
  (the identical mechanism retention's dismiss prefix already depends on). The
  decrement stays in `runSearch`'s JS `finally`, so it always runs on success/reject.

- **D4 — Leg G (`runBounded`) is FROZEN.** Do not touch files.ts:352-363. Its
  bounding/kill contract — shared exact byte cap over stdout+stderr, immediate SIGKILL
  on timeout (no TERM grace), settle-only-after-child-reap, raw output on nonzero exit,
  finite stdin — is pinned by files-run-bounded.test.ts and must remain byte-identical.
  Legs B/D call it unchanged; walkSearch's `yieldToLoop`/`WALK_YIELD_EVERY=512`
  (Leg E/H) and the binary-sniff/`READ_MAX` newline-clamped truncation (Leg F) are
  likewise carried verbatim into the cores.

- **D5 — transport-500 LOG-shape delta (search only).** A search-backend rejection
  becomes an `Effect.promise` die; `runControlDetached` rejects with a `FiberFailure`
  wrapping the defect. `settleFilesystemOperation.catch` logs `err` and emits 500
  `{ok:false,reason:'internal'}`. **The body is byte-identical; only the logged object
  changes** (raw error → FiberFailure). This matches the control-group precedent that
  only response bytes are contractual and shutdown/defect log lines may differ. See Q4
  for the byte-identical-log fallback if the orchestrator wants it.

- **D6 — Leg A vs Leg C rejection asymmetry MUST be preserved.** listAt's git-ignore
  await is INSIDE its `try/catch` → a rejection → `failure(err)` → 404 (handled in the
  core). searchAt's backend await is inside a `finally` with NO catch → a rejection
  propagates → transport 500. `listAtEffect.runIgnore` therefore keeps its inner
  `try/catch → failure`; `searchAtEffect.runSearch` keeps `try/finally` with NO catch.

- **D7 — shared-with-landed-code.** No non-HTTP domain caller exists (§1.0), so the
  cores carry no cross-package obligation. BUT `settleFilesystemOperation` is SHARED by
  all six routes and by nothing else; it stays byte-identical (unchanged file), so the
  `.then(json)/.catch(500)` dialect is preserved across every slice. `fleetd.bundle.mjs`
  is a build artifact — rollback (flag flip) needs no bundle edit; a release rebuild
  regenerates it.

---

## §5 OPEN QUESTIONS (for the orchestrator)

- **Q1 — Keep the FS GET routes OFF the P6.4 `runRequest` transport?**
  Recommendation: **YES.** Convert cores + discharge via `runControlDetached`, leave
  `settleFilesystemOperation` untouched. This is the completion map's P9.3/P9.5 split
  (P9.5 owns the `runRequest` + read-settler move, rows G5/G6) and matches the P9.1
  precedent for already-async routes. Bringing reads onto `runRequest` (a new
  `settleEffectReadRoute` with snapshot-style quiesce→legacy-replay — safe for reads
  since replay re-reads, no write) is a coherent P9.5 design but is NOT P9.3.

- **Q2 — Discharge the pure-sync `readAt` through `runControlDetached` at all?**
  Recommendation: **YES, for uniformity.** readAt is already `async` today; a
  `runControlDetached(Effect.sync(...))` resolves on a microtask, behaviorally
  identical. All three ops then share ONE dispatch idiom and ONE rollback flag.
  Alternative (return `Promise.resolve(readAtEffect run synchronously)`) saves a
  runtime hop but forks the dispatch shape — not worth it.

- **Q3 — Single `EFFECT_CORE_FILES` flag vs per-op flags?**
  Recommendation: **single flag**, per-op dispatcher migration across Slices 1-3
  (matches the `EFFECT_CORE_DISMISS` precedent). Per-op flags add three kill-switches
  for a three-op file with no independent-rollback need. If the orchestrator wants
  per-op rollback granularity, splitting into `EFFECT_CORE_FILES_{READ,LIST,SEARCH}`
  is mechanical.

- **Q4 — Accept the D5 log-shape delta, or preserve byte-identical logs?**
  Recommendation: **accept** (body is the contract; log is diagnostic; consistent with
  the landed control group). If byte-identical logs are required, wrap `runSearch` to
  `catch (err) { console.error(\`fleetd … filesystem error:\`, err); throw err }` so the
  RAW error is logged before the die — but that pre-logs inside the core and then still
  double-logs at the transport, so it is strictly worse; the cleaner byte-identical
  option is to have `runSearch` return the 500 wire itself (folding the rejection, like
  `controlAsyncWorkflow`) and drop reliance on the transport `.catch`. Flag if wanted.

- **Q5 — `RunControlDetached` type: local alias vs shared import?**
  Recommendation: **local alias** in files.ts (zero new import edge; mirrors how
  ingress-supervisor and retention each declare it). Promote to a shared type only if a
  third consumer appears.

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-23)

**Q1 ACCEPTED:** the three GET route families stay OFF the P6.4 transport in P9.3; `settleFilesystemOperation` stays byte-identical; the transport/read-settler move is P9.5 (rows G5/G6).

**Q2 ACCEPTED:** discharge the pure-sync `readAt` through the runner too — uniformity over micro-optimization.

**Q3 ACCEPTED:** one `EFFECT_CORE_FILES` flag for the family.

**Q4 REJECTED — the premise is wrong:** `runPromiseWith` rejects with `causeSquash(exit.cause)` = the RAW first Die defect / Fail error, identity-preserved, no FiberFailure wrapper (verified: node_modules/effect/src/internal/effect.ts:299-309, :5475-5489). No log-shape delta is expected on the search 500; byte parity is REQUIRED for the log line as well as the body. If a delta is actually observed in a test, root-cause where the wrapping is introduced before accepting any drift — do not wave it as diagnostic-only.

**Q5 ACCEPTED with preference order:** reuse the exported `RunControlDetached` type from retention.ts if it imports without adding a forbidden edge (check import-boundaries); otherwise the local structural alias.

**D3 GAP endorsed:** the `searchesInFlight` sync-admission pin (injected-runner helper) must land BEFORE the search core converts (slice-3 precondition, same as P9.1's characterization-first rule).

**Reviews:** slice 3 (search, the cap + BUG-027 retry) = adversarial-review-mandatory; slices 1–2 = orchestrator line review unless the worker reports surprises.

**Sequencing:** P9.3 may be implemented in the worktree in parallel with P9.1's close (files.ts does not overlap slice 6b); integration/cherry-pick onto fd/v1-effect-feasibility happens only after P9.1 wraps.
