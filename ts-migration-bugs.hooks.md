<!-- STAGING for scripts/fleet-*.ts (the three plugin hook shims) — merge into
     ts-migration-bugs.md (newest-first, right below the "entries appended below"
     marker) once the concurrent board subagent has finished its own append. Kept
     separate only to avoid a concurrent-append race on the shared log. -->

### scripts/fleet-hook.ts + scripts/fleet-sessionstart.ts + scripts/fleet-watch.ts — the three plugin hook shims (~550 loc): NO real bug, one instructive TS flow-analysis limitation surfaced-and-worked-around, otherwise all NOISE, every security invariant intact   [ALL NOISE / TS-LIMITATION]

- **What:** the three command-hook shims Claude Code runs — `fleet-hook.ts` (authenticated
  shim for every hook event except SessionStart + the Stop rewake leg), `fleet-sessionstart.ts`
  (the ONE election+spawn+takeover+brief hook), `fleet-watch.ts` (the F3d-2 asyncRewake Stop
  watcher). Converted all three `.mjs` → `.ts` under root maximal-strict, then added a
  `bundle:hooks` esbuild step producing self-contained committed `.mjs` artifacts (inlining the
  four `scripts/fleetd/` deps — config.ts, run-nonce.ts, env-scrub.ts, takeover.ts — which
  import only node builtins, so the bundles carry zero bare imports). New CI drift gate
  (`bun run bundle:hooks` + `git diff --exit-code` on the three artifacts). `tsc` (root
  maximal-strict) + type-aware `eslint` (`--report-unused-disable-directives`) driven to **0**,
  control-bytes 0 on both sources and artifacts, **zero intended runtime move**.

- **Every security surface preserved verbatim:** the LOCKSTEP hold-window invariant
  (`HOLD_EVENTS` → 660 s watchdog under hooks.json's 720 s), the re-armable SessionStart
  watchdog (3.8 s → 8 s on takeover, always < hooks.json's 15 s ceiling), run-nonce tagging
  (BUG-025, keyed on CLAUDE_PID), the takeover identity gates (`shouldTakeOver` +
  `verifyDaemonPid` + managed-daemon no-evict + `replacementMatches` re-arbitration with the
  round≥1 anti-flap cap), the `bootEnv` scrub list (Claude/agent markers + gateway + spawn +
  the test seams that must never ride a tmux server's global env — the 2026-07-11 ghost-daemon
  scar), the 0600 chmod-on-open of `fleetd.log`, the detached-spawn `child.once('error', …)` +
  `unref()` + fd release in `finally`, the single-flight NEWEST-WINS pid ownership + BUG-105
  `wg` generation token + BUG-034 `/mail/ack` lease + 64 KB/1 MB stdin ceilings + listener
  cleanup in fleet-watch, and the silent-exit-0 failure contract in all three. Artifacts keep
  their `fleet-*.mjs` basenames, so **hooks.json needs no repoint** (verified: it still names
  `fleet-hook.mjs` / `fleet-sessionstart.mjs` / `fleet-watch.mjs`), and `spawn` stays an
  external named `node:child_process` import in the sessionstart artifact (the audit spawn-error
  test's `syncBuiltinESMExports()` depends on that).

- **THE INSTRUCTIVE FINDING — TS narrows a closure-mutated module `let` to its initializer
  (`fleet-sessionstart.ts`, worked around, NOT a runtime bug):**
  - **Where:** `let replacedVersion = null` / `let managedVersionDrift = null` at module scope,
    assigned ONLY inside `ensureServer()` (`replacedVersion = health.version` on a committed
    takeover; `managedVersionDrift = health.version` on a managed-daemon drift), then READ at
    top level after `await ensureServer()` (`if (replacedVersion) payload.fleet_takeover = …`;
    `if (managedVersionDrift) { … v${managedVersionDrift} … }`).
  - **The symptom:** with a plain `let x: string | null = null`, TS flow analysis narrows every
    top-level read to the literal `null` — it cannot see the assignment across the
    `ensureServer` closure boundary (a call does not reset a local's narrowing, and the
    "assigned-in-nested-function ⇒ widen to declared type" heuristic only fires for reads
    *inside* a nested function, not for reads in the declaration's own scope). So
    `no-unnecessary-condition` reports the real takeover guards as **"always falsy"**, and the
    `if`-body narrows `managedVersionDrift` to **`never`**, which `restrict-template-expressions`
    then rejects inside the drift-warning template. `tsc` itself stayed green — `null`-in-a-
    condition and `never`-in-a-template are lint findings, not type errors — so this is exactly
    the class of latent mis-modeling maximal-strict + type-aware lint is meant to expose.
  - **Why it is NOT a bug:** the runtime is correct — `ensureServer()` really does set both
    before the reads; only the *type checker's* model was wrong. Confirmed empirically in a
    throwaway scratch: `let x: string|null = null` reproduces the always-falsy/never errors;
    `let x = null as string | null` does not.
  - **Fix (faithful, zero runtime move):** widen the initializer with `null as string | null`
    so the read-site type stays `string | null` (guard necessary, `if`-body narrows to
    `string`), with a comment naming the cross-closure assignment as the reason. Chosen over a
    blanket `no-unnecessary-condition` disable because the cast *documents the shared-state
    reality* and keeps the guard genuinely type-checked, and over restructuring
    `ensureServer` to return the values because `replacedVersion` is consumed mid-function by
    `bootEnv()` (it must be module state, not a return value).

- **Why the rest is noise:**
  1. **`no-dynamic-delete` — `delete env[k]` in `bootEnv`** (sessionstart). The scrub loop
     deletes a *dynamic* key list; `delete` (not `env[k] = undefined`) is load-bearing — it
     guarantees the key is ABSENT from the child's env rather than passed as an empty string.
     Scoped disable-with-reason.
  2. **TS4111 ×2 — index-signature dot access.** `noPropertyAccessFromIndexSignature` requires
     `process.env['FLEETDECK_TEST_DAEMON_SCRIPT']` and `env['FLEETDECK_REPLACED']` (ProcessEnv is
     an index signature). Pure syntax; no behavior.
  3. **`prefer-nullish-coalescing` on load-bearing `||` ×2** (sessionstart). `FLEETDECK_TEST_DAEMON_SCRIPT || bundle`
     (an empty/unset test seam must fall back to the bundle, not be kept as `''`) and
     `payload.hook_event_name || 'SessionStart'` (a missing OR empty event name must default) —
     `??` would keep `''`, a behavior change. Kept `||` with scoped disables, both confirmed
     load-bearing via `--report-unused-disable-directives`. (fleet-hook.ts carries the same
     idiom on `withRun(raw) || '{}'`.)
  4. **exactOptionalPropertyTypes forbids `headers: undefined` / `body: undefined`.** In
     sessionstart's `api()` the request body is spread in only when present
     (`...(body ? { body: JSON.stringify(body) } : {})`) and `headers` is always an object
     (empty ≡ no headers on the wire); fleet-watch/​fleet-hook use the sibling shared-object
     (`authHeaders`) / typed-object patterns. No explicit-undefined property anywhere.
  5. **Wire-JSON typed as trusted shapes, cast to keep `?.` guards necessary.** `api<T>()`
     returns `(await res.json()) as T` with `T` = `Health` / `Registration` / null; the daemon
     has minted these fields at every boot, so a mismatch reproduces the pre-migration untyped
     behavior rather than a new failure mode (same idiom as bin/fleetdeck.ts's `as Health`).
     `Registration.upgrade_lines`/`.brief` stay `unknown` and are re-validated at the read site
     (`Array.isArray`, `typeof brief === 'string'`) — the daemon-garbage defense the `.mjs`
     already had. In **fleet-watch.ts** the same rule bit the other way: casting `res.json()` to
     a *non-null* `WatchResponse` let TS prove `out` never-null and turned the runtime null-body
     guards into `no-unnecessary-condition` errors → fixed by casting to `WatchResponse | null`.
  6. **`restrict-template-expressions` bans `null`/`any` in templates.** `String(ownVersion())`
     (ownVersion is `string | null`) and `${String(line)}` (an `any` upgrade line off the
     Array.isArray narrowing) coerce explicitly; both are clean under `no-base-to-string`
     (neither is an object type). `String()` of the wire values matches the `.mjs`.
  7. **Trivia:** fleet-watch dropped a dead `let out = null` init (`no-useless-assignment`) and a
     single-assignment `let timer` → `const timer` (`prefer-const`); the sessionstart poll uses
     `await new Promise<void>((r) => { setTimeout(r, 250); })` (braced executor); `child.once('error', …)`
     carries a comment inside the empty handler (`no-empty-function`) — all sibling idioms, no
     runtime move.

- **Fix:** all in place. Isolated `tsc` clean on the three sources; `eslint
  --report-unused-disable-directives` exit 0 (every disable load-bearing); control-bytes 0 on
  sources AND the regenerated artifacts; artifacts self-contained (node-builtin imports only),
  shebang line 1 + banner line 2 preserved. `bundle:hooks` npm script + CI drift gate added.
  Hook-affected suites green: `accept-scripts-hook-wiring` + `audit-hardening` + `hook-auth` +
  `filechanged-watch` (33/0), `watch-rewake` + `max-turns-abort` (19/0), `takeover` +
  `smoke-project-isolation` (17/0), `plugin-payload-gate` + `release-gate` (20/0) → **89 pass /
  0 fail** against the freshly-bundled `.mjs` artifacts.
