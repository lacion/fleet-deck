<!-- STAGING for spawns.ts — merge into ts-migration-bugs.md (newest-first, right below the
     "entries appended below" marker) once the concurrent board subagent has finished its own
     append. Kept separate only to avoid a concurrent-append race on the shared log. -->

### spawns.ts — the board-spawn lifecycle giant (~3270 loc): all NOISE, no latent bug, security invariants intact   [NOISE]
- **What:** the largest daemon module (spawn / revive / harvest / kill / setup-wrapper). `tsc`
  (root maximal-strict) + type-aware `eslint` driven to **0**, control-bytes 0, **zero runtime move**.
  Every finding was ergonomics of maximal-strict on dynamic JS or a lint idiom already established by
  sibling `derive.ts`/`settings.ts`. The security-critical surfaces were preserved verbatim:
  `spawnFailureReason` still routes through `redactGitText`; `stallDiagnosticExcerpt` keeps its
  scrub/redact + 2 KB bound with `exactSecrets`; the arm gate, gateway/remote-control mutual
  exclusion, `SETUP_CONTROL_RE` shell-injection guard, and prompt-as-last-argv all unchanged.

- **Why it's noise (the interesting ones):**
  1. **`SpawnsCtx` method-shorthand members tripped `unbound-method` when destructured.** `createSpawns`
     destructures the ctx (`const { tick, logEvent, onMutate, … } = ctx`); with method-shorthand
     interface members (`tick(msg): void`) the rule fires on the free reference. Every member is a
     plain closure wired by derive's `CoreCtx` and none reads `this`, so the honest structural fix is
     arrow-property syntax (`tick: (msg: string) => void`) — exactly what the sibling `CoreCtx` already
     uses (derive.ts:183/189). Converted the interface; no disable, no runtime move.
  2. **`no-base-to-string`: `?? ''` widened `unknown` → `{}`.** `spawnFailureReason` builds
     `String((err as {message?: unknown} | null)?.message ?? err ?? '')`. The `?? ''` widened the
     operand to `{} | string`, tripping `no-base-to-string` — even though the sibling `errMessage`
     does `String(err)` on bare `unknown` cleanly. Fix: annotate the coalesced value `: unknown` so
     `String()` stays the sanctioned coercion. Behavior-identical.
  3. **`setup_cmd` insert paths disagreed on `|| null` vs `?? null`.** `prefer-nullish-coalescing`
     surfaced that the primary path (`const setupCmd = body.setup_cmd ?? null`, :928) used `??` while
     two other insert paths (:1447, :1695) used `|| null`. The only divergence is when a client POSTs
     `setup_cmd: ""`: `||` stores `null`, `??` stores `""`. Confirmed **every** consumer keys off
     truthiness — env injection `setupCmd ? {FLEETDECK_SETUP_CMD…} : {}` (:931), `!!row.setup_cmd`
     (:2886), `row.setup_cmd ? … : …` (:2911) — so `""` and `null` are functionally identical
     downstream. Harmonized all three to `?? null`. No behavior move; the setup command is still
     inert when empty.
  4. **HTTP-status fallbacks `errStatus(err) || N` → `?? N`.** `errStatus` returns `number | undefined`
     and never a real `0`, so the operators are behavior-identical here; switched to `??` to match the
     established `errStatus(err) ?? 500` idiom in settings.ts:755. Same for object/`undefined`
     fallbacks (`gatewayEnv ?? {}`, `activeSpawnBySession.get() ?? provisioning…`) and empty-string
     folds that are identical either way (`process.env['SHELL'] ?? ''`, `body.cwd ?? ''`,
     `forbidden ?? 'worktree'`, kill-result `error ?? '…'` — `KillResult.error` is only ever unset or
     a non-empty diagnostic).
  5. **`no-misused-promises`: `setTimeout(async () => {…})`.** The stall-nudge scheduled an async
     callback (a floating promise). Extracted the async body verbatim into `const nudge = async () => {…}`
     and scheduled `setTimeout(() => void nudge(), NUDGE_MS)` — the timer callback now returns `void`;
     the body (fully try/caught, never rejects) is untouched. Detach callbacks in the repo-provision
     path got the same `void`-operator treatment; empty best-effort `.catch(() => {…})` bodies got an
     explanatory comment (sibling idiom, derive.ts:1290).
  6. **`use-unknown-in-catch-callback-variable` / `prefer-optional-chain`.** `.catch((err: unknown) => …)`
     annotation; `!row || row.status !== 'spawning'` → `row?.status !== 'spawning'` (and the twin
     `SessionEnd` guard) — the narrowing still flows because `=== 'literal'` proves non-null.

- **Fix:** all in place; no runtime behavior moved. Importers repointed to `.ts`
  (`http.mjs:20`, `tests/daemon-maintenance.test.mjs:10`, `tests/git-stderr-detail.test.mjs:432`).
  Affected suites green (see commit).
