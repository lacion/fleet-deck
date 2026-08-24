# P9.6 design draft — `src/daemon/takeover.ts` (takeover / election lifecycle)

> **Stamped into the repo 2026-08-24.** Copied verbatim from the adjudicated draft `/tmp/fd-effect/p9-6-design-draft.md`; §6 carries the orchestrator's rulings. Adjudicated a justified NON-conversion — no source change; the takeover bridge (`acquireDaemonResourcesOwned` + its three enforcing gates) is inventoried in `p9-close.md`.

**Status:** design/inventory only — no source changed. Worktree `/tmp/fd-wt-p9-4-design`
(`tmp/p9-4-design` @ `0a18335c`). All line anchors are from that tree.

**Headline recommendation (the §2 crux, stated up front):**
**Takeover is already inside the P9 exit gate's adapter allowance. It needs
inventory + a one-line bridge entry, and NOT a source conversion.** The module's
single async leg is already discharged from an Effect fiber through a *named*
Promise adapter (`live-layer.ts` `Effect.callback`), with an Effect-supplied
`AbortSignal`. Converting `takeover.ts` itself to import `effect` is not merely
unnecessary — it is **forbidden by three green test gates** and by the module's
own dependency-free contract (it is inlined verbatim into a hook bundle that must
contain zero Effect markers). This draft documents that with evidence rather than
inventing conversion work.

---

## §1 — Async inventory (lettered)

`takeover.ts` is 352 lines. It is overwhelmingly **pure, synchronous**
predicates; its async surface is a single function.

### The one async leg

**(A) `terminateDaemon(pid, { timeoutMs = 2000, sleep = defaultSleep, signal })
: Promise<boolean>`** — `takeover.ts:298`.
- Sends `process.kill(pid, 'SIGTERM')` (`:308`), then polls `kill(pid, 0)` in
  `stepMs = 100` slices until `ESRCH` or `timeoutMs` elapses (`:316-347`).
- **Injected `sleep`** (`:302`, default `defaultSleep = sleep` from
  `helpers.ts` at `:288`/`:34`) — the poll delay; injectable so tests drive it
  without real time.
- **Optional `AbortSignal`** (`:303`) — `signal?.throwIfAborted()` up front
  (`:306`) and a per-slice `addEventListener('abort', …)` race (`:322-344`) so a
  root-acquisition cancel unwinds the poll immediately.
- **Return contract is deliberately errorless / fail-open:**
  - `true` = pid gone → clean handoff (also `ESRCH` on the initial kill, `:311`).
  - `false` = wedged daemon that ignored SIGTERM (`:350`) **or** `EPERM`/other
    (`:314`) → caller fails open, never force-kills. **No SIGKILL escalation**
    anywhere (`:23`, `:348-349`).
  - The **only** thrown value is a native `AbortError` (`DOMException`)
    propagated from `throwIfAborted()` at `:306` / `:333-335`. That is the
    interruption channel, not a domain error.

### Everything else is synchronous / pure (no async, for completeness of the inventory)

All import nothing beyond `node:fs`, `node:path`, `errCode` (`errors.ts`), and
(transitively, for `defaultSleep`) `helpers.ts`:
- `pidRecord`, `pidIsLive` (`process.kill(pid,0)` probe, `:69`),
  `fleetdProcessIdentity`, `livePidLooksLikeFleetd` — pidfile + `/proc` identity.
- `parseSemver`, `isZeroVersion`, `compareSemver`, `shouldTakeOver` — the
  strictly-newer-only + `0.0.0` sentinel loop-guard election math.
- `verifyDaemonPid` — the trust gate before accepting output or sending SIGTERM
  (`:234`).
- `replacementMatches` — the post-spawn build-identity gate.

**Async-leg count for the module = ONE (`terminateDaemon`). `q.*` sites = ZERO.**
This matches the completion map's row: "takeover.ts — 0 `q.*`; typed lifecycle,
not Store" and its P9.6 leftover "none (q.\*=0)".

### Where (A) is invoked — the three consumers

1. **Daemon boot (Effect runtime).** `program.ts` election:
   `supersedeIfNewer(record)` (`:408`, BUG-156) → `shouldTakeOver` (`:439`) →
   `verifyDaemonPid` (`:441`) → `await terminateDaemon(record.pid, { signal })`
   (`:442`), inside `bootDaemon(signal, …)` (`:182`) reached from
   `acquireDaemonResources(signal, …)` (`:1342`). **This whole Promise is already
   discharged from an Effect fiber** — see §2.
2. **CLI serve preflight (non-Effect).** `bin/fleetdeck.ts` ships its **own
   inlined** `terminateDaemon` (`:90`, used `:600`, `:740`) alongside the
   load-bearing systemd exit codes `EXIT_WRONG_RUNTIME = 78` (`:213`) and
   `EXIT_INCOMPLETE_INSTALL = 66` (`:214`). Plain Node CLI. Not Effect.
3. **SessionStart hook (non-Effect, source-inlined).**
   `scripts/fleet-sessionstart.ts` imports
   `{ shouldTakeOver, verifyDaemonPid, terminateDaemon, replacementMatches }`
   from `../src/daemon/takeover.ts` (`:26-31`) and calls them in plain-Promise
   `ensureServer(round)` (`:266`, `terminateDaemon` at `:299`). **No `effect`
   import.** The release bundle inlines `takeover.ts` into `fleet-sessionstart.mjs`.

---

## §2 — Target shape (the crux: honest minimal Effect ownership)

### The classification

`takeover.ts` is a **dependency-free lifecycle-contract module** (node builtins +
sibling source only) shared across **one Effect runtime and two non-Effect
runtimes**. Under the P9 taxonomy it is *not* ROOT-CONVERTIBLE (yields no Store,
0 `q.*`) and *not* an HTTP capability. It is a **P9-SEAM adapter body**: the
imperative implementation behind a named Promise adapter.

### The P9 exit gate, quoted

> all daemon asynchronous application **workflows** are Effects; the remaining
> Promises are native callback/Response values living inside **named adapters**;
> every compatibility **bridge** is inventoried.

`terminateDaemon` is not an *application workflow* — it is a process-lifecycle
primitive (SIGTERM + kill-poll). The gate's own text sanctions it living as a
native Promise **inside a named adapter**, provided that adapter is inventoried.

### That adapter already exists and is already reviewed

`live-layer.ts` `acquireDaemonResourcesOwned` (`:211`) wraps daemon boot in:
```
Effect.callback<…>((resume, signal) => {          // :217  — named adapter, Effect-supplied signal
  const acquisition =
    Promise.resolve(options.acquireDaemonResources(signal, ingress, inputs));  // :228
  …
})
```
called inside `Effect.gen` at `:448`. So the path
`Effect fiber → Effect.callback → acquireDaemonResources(signal) → bootDaemon →
terminateDaemon(pid, { signal })` is **already** how the daemon reaches (A): a
native Promise, inside a named `Effect.callback` adapter, cancelled by the
Effect-owned `AbortSignal`. The signal wiring is exercised end-to-end and green
(`tests/effect/signal-during-acquisition.test.ts` — SIGTERM mid-acquisition exits
0, never evicts the incumbent, never writes ownership).

### Why converting `takeover.ts` is forbidden, not just unnecessary

Adding `import … from "effect"` to `takeover.ts` breaks **three green gates** and
the stated dependency-free contract (`takeover.ts:29` "MUST stay dependency-free —
node builtins only — to work unbundled"):

1. **Hook-artifact Effect-marker scan** — `import-boundaries.test.ts:980`
   ("generated board and every hook artifact contain zero Effect runtime
   markers"). `takeover.ts` is inlined into `fleet-sessionstart.mjs`; an `effect`
   import would emit an `external Effect module import` marker → **fail**. *This
   is the decisive gate.*
2. **Fail-open floor stays Effect-free** — `import-boundaries.test.ts:772` +
   `FLOOR_SEAM_ALLOW = { config, run-nonce, env-scrub, takeover }` (`:90`). The
   hook may import `takeover` but must stay Effect-free; pulling Effect through
   the seam defeats the floor's purpose.
3. **Bin self-containment** — `cli-serve-paths.test.ts:235` pins `terminateDaemon`
   (+ `pidRecord`/`pidIsLive`/`livePidLooksLikeFleetd`) **inlined verbatim** into
   `bin/fleetdeck.mjs` with no runtime `takeover`/module load; the exit-78
   wrong-runtime guard (`:~205`) must run without importing the daemon/Effect
   bundle at all.

### The honest target shape

**No new Effect core. No `*Legacy` twin. No kill-switch flag.** The target shape
for P9.6 is an **inventory delta only**:

- **T1 (bridge inventory).** Record `terminateDaemon` as a P9-SEAM adapter body in
  `p9-completion-map.md` / the bridge ledger: *"native process-lifecycle Promise;
  consumed from an Effect fiber via the `acquireDaemonResourcesOwned`
  `Effect.callback` adapter (`live-layer.ts:217`) with an Effect-owned
  `AbortSignal`; two additional non-Effect consumers (CLI serve preflight, hook)
  by design — module is dependency-free/source-inlined."* This is the "every
  compatibility bridge is inventoried" clause, satisfied on paper.
- **T2 (optional, typed-error surfacing — see §5).** The adapter boundary in
  `live-layer.ts`, *not* `takeover.ts`, is the only place a typed error could be
  introduced. And even there the surface is nearly empty: (A)'s contract is
  `boolean` (fail-open) with a single native `AbortError` for interruption, which
  `Effect.callback` already maps to fiber interruption. There is essentially
  nothing to "typed-error-ify" — the boolean *is* the typed result.

**Net: takeover.ts source is unchanged. Ownership lives at the already-existing,
already-reviewed consumer adapter.**

---

## §3 — Slice plan

Because the honest target is inventory-only, there is **no source-conversion slice
ladder**. The plan is one documentation slice plus an explicit no-op record.

- **Slice 0 — characterization (already covered).** The pins that freeze
  takeover's contract are green today: `takeover.test.ts` (19 pass — semver
  rules, `verifyDaemonPid` trust gate, BUG-179 cross-HOME guard, BUG-156
  older-build refusal, MANAGED never-evicted, **SIGTERM-immune stale daemon fails
  open with no SIGKILL**), `takeover-abort.test.ts` (1 pass — abort unwinds the
  poll in <500ms and never escalates to SIGKILL), `election.test.ts` (8 pass —
  claimHome/supersedeIfNewer boot election, exit-code 3 loser, BUG-164),
  `tests/effect/signal-during-acquisition.test.ts` (the Effect-boundary
  integration). No new characterization test is required; the contract is already
  frozen at the wire.
- **Slice 1 — bridge inventory (T1).** Add the one-row P9-SEAM ledger entry
  described in §2/T1. Pure documentation. No code, no test change.
- **Slice 2 — (conditional, only if the reviewer wants it) adapter-side error
  note (T2).** A comment/inventory note at `live-layer.ts:217-228` stating the
  `AbortError → interruption` and `boolean → plain value` mapping. Still no
  `takeover.ts` change, no flag, no twin. **Recommended: fold into Slice 1's
  prose rather than touch `live-layer.ts`.**

**Slice count: 1 (inventory), with Slice 2 optional and recommended-merged.**
This is a justified **non-conversion**, not a deferral.

---

## §4 — Danger notes

- **D1 — The dependency-free contract is load-bearing and test-enforced.** Any
  instinct to "just wrap it in an Effect" inside `takeover.ts` trips
  `import-boundaries.test.ts:980` (hook Effect-marker scan) and violates the
  `:29` header contract. The unbundled first-run SessionStart hook imports this
  file as *source* under Bun type-stripping before any bundle exists — Effect must
  never be on that path.
- **D2 — Exit codes 78 / 66 are a systemd contract, not takeover's to move.**
  `bin/fleetdeck.ts:213-214` (`EX_CONFIG` / `EX_NOINPUT`). The CLI's inlined
  `terminateDaemon` copy and these codes are the wrong-runtime / incomplete-install
  landmine guards. P9.6 must not "unify" the CLI copy into an Effect path — that
  would re-introduce the exact `Cannot find module …/takeover.ts` class of crash
  the bin-inline test guards against (`cli-serve-paths.test.ts:220-259`).
- **D3 — Fail-open semantics must survive any error-surfacing.** (A) folds
  `ESRCH`→true, `EPERM`/wedged→false **on purpose** (`:310-314`, `:348-350`). If a
  future reviewer surfaces a typed error at the adapter, it must **not** turn a
  `false` (wedged, keep serving) into a failed Effect — a SIGTERM-immune stale
  daemon staying up is the *designed* safe outcome (`takeover.test.ts:830`
  pins it). The only value that should become interruption is `AbortError`.
- **D4 — No SIGKILL escalation, ever.** Both the wedged path (`:348`) and the
  abort path (`takeover-abort.test.ts:45`) assert the incumbent survives. Any
  Effect-side "retry/timeout then force" scheduling would violate the contract.
- **D5 — Verify-before-kill ordering is inside the caller, not (A).**
  `verifyDaemonPid` (`program.ts:441`) gates `terminateDaemon` (`:442`). The
  adapter must preserve that ordering; it already does (both are plain awaits in
  the same Promise the `Effect.callback` discharges).

---

## §5 — Open questions (with recommendations)

- **Q1 — Do we want *any* `live-layer.ts` edit for T2, or is prose enough?**
  **Recommendation: prose only.** (A)'s only throwable is `AbortError`, which
  `Effect.callback`'s `signal` already routes to interruption; the `boolean` is a
  plain domain value the caller already branches on. A typed `TakeoverError` would
  be a channel with no members. Document the mapping in the bridge ledger; do not
  touch `live-layer.ts`.

- **Q2 — Should the completion map's P9.6 row be marked "converted" or
  "sanctioned native adapter"?** **Recommendation: "sanctioned native adapter
  (inventoried)"**, mirroring how HTTP-capability rows are recorded — with the
  three enforcing tests named so a future reader sees the conversion is
  *prohibited*, not *pending*.

- **Q3 — Does the CLI's duplicate `terminateDaemon` (`bin/fleetdeck.ts:90`) count
  as an un-inventoried bridge for the exit gate?** **Recommendation: inventory it
  as an intentional, esbuild-inlined copy** (pinned by `cli-serve-paths.test.ts`),
  not a bridge to converge. It exists precisely because the shipped CLI must carry
  zero runtime source/Effect dependency. Converging it is out of scope for P9 and
  would regress the incomplete-install guard.

- **Q4 — Is there a coverage gap around the Effect-fiber cancel of (A)?**
  **Assessment: no material gap.** `takeover-abort.test.ts` pins the
  `AbortSignal` unwind at the unit level and `tests/effect/signal-during-acquisition.test.ts`
  pins it end-to-end through the real `Effect.callback` adapter (SIGTERM
  mid-acquisition → exit 0, incumbent alive, no partial ownership). If the
  reviewer wants belt-and-suspenders, the *only* addition worth making is an
  assertion that the adapter surfaces `AbortError` as Effect **interruption**
  (not failure) — but that tests `live-layer.ts`, not `takeover.ts`, and is
  arguably already implied by the exit-0 clean-shutdown assertion.

---

### One-line conclusion

P9.6 is a **justified non-conversion**: `takeover.ts` is a dependency-free,
node-builtins-only lifecycle module whose single async leg (`terminateDaemon`) is
already consumed from an Effect fiber through the reviewed
`acquireDaemonResourcesOwned` `Effect.callback` adapter with an Effect-owned
`AbortSignal`. Converting the module is forbidden by three green gates. The exit
gate is satisfied by **inventorying the existing named adapter**, not by touching
source.

---

## §6 — ORCHESTRATOR ADJUDICATION (Fable, 2026-08-24)

**§2 ANSWER ACCEPTED IN FULL — justified non-conversion.** takeover.ts stays byte-untouched: its one async leg is already discharged through the reviewed named adapter (acquireDaemonResourcesOwned → Effect.callback with an Effect-owned AbortSignal), the errorless Promise<boolean> fail-open contract is the design (false = keep the wedged incumbent serving — NEVER a failed Effect), and the dependency-free constraint is load-bearing and test-enforced (hook Effect-marker scan, fail-open floor, exit-78/66 self-containment). Q1 prose-only, Q2 "sanctioned native adapter (inventoried)" row naming the three enforcing tests, Q3 pinned intentional inline, Q4 no gap — all ACCEPTED as recommended.

**Slice plan RULED:** ONE documentation slice, folded into the P9 close-out docs pass (this draft stamps into docs/v1/evidence/effect/p9-6-design.md; the migration ledger's bridge inventory gains the takeover row). No adversarial review needed — there is no code change to review.
