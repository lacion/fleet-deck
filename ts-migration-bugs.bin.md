<!-- STAGING for bin/ (fleetdeck.ts + tmux-version.ts) — merge into ts-migration-bugs.md
     (newest-first, right below the "entries appended below" marker) once the concurrent
     board subagent has finished its own append. Kept separate only to avoid a
     concurrent-append race on the shared log. -->

### bin/fleetdeck.ts + bin/tmux-version.ts — the install/serve/service CLI (~1010 loc): one PRE-EXISTING latent bug surfaced-and-preserved, otherwise all NOISE, every security invariant intact   [LATENT BUG — preserved]

- **What:** the security-critical CLI (env-file + systemd-unit writing, supervisor/stop
  identity gates, token rotation, tmux-version probe). Converted `bin/fleetdeck.mjs` →
  `bin/fleetdeck.ts` and `bin/tmux-version.mjs` → `bin/tmux-version.ts`, then bundled the CLI
  back to a committed self-contained `bin/fleetdeck.mjs` artifact (`bun run bundle:bin`, new
  CI drift gate). `tsc` (root maximal-strict) + type-aware `eslint` driven to **0**,
  control-bytes 0, **zero intended runtime move**. Every security surface preserved verbatim:
  env-value BARE-SAFE/UNQUOTABLE quoting (read by both POSIX `.`-source and systemd
  `EnvironmentFile`), 0600 chmod-on-write, `%`→`%%`-first systemd escaping, always-quote
  ExecStart args refusing control/`"`, EnvironmentFile-path refusal of whitespace/quotes/
  backslash, POSIX single-quote `shQuote` (BUG-079 no-expansion), the SUPERVISOR-IDENTITY
  (`argvIsOurSupervisor` via /proc cmdline) and STOP-TARGET-IDENTITY
  (`healthIsOurManagedDaemon`/`healthPidIsOurDaemon` → managed + FLEETD_PID ownership + port
  match + `verifyDaemonPid`) contracts, 0600 token rotation, lazy (never top-level) takeover
  import, `resolveCliPort` 1..65535 clamp, and ExecStart/SUPERVISE `path.join(HERE,
  'fleetdeck.mjs')` (the artifact must keep that basename).

- **THE LATENT BUG — supervised `service start` accepts a foreign responder (surfaced by strict
  typing, deliberately NOT fixed):**
  - **Where:** `serviceStart()` non-systemd branch, `bin/fleetdeck.ts:775`:
    `const h = await waitForHealth({ expect: healthIsOurManagedDaemon });`
  - **The defect:** `healthIsOurManagedDaemon` is `async` (`:714`, returns `Promise<boolean>` —
    it must be, it `await import()`s takeover's `verifyDaemonPid`). `waitForHealth`'s loop tests
    the predicate **un-awaited**: `if (h && (!expect || expect(h))) return h;` (`:700`). A
    Promise is *always* truthy, so `expect(h)` is satisfied for **any** non-null health answer —
    the managed-identity gate is effectively a no-op inside the wait loop.
  - **Consequence:** on the supervised (no-systemd) path, if an **unmanaged/foreign** daemon
    already owns `:PORT`, `waitForHealth` returns that squatter's health on the first probe,
    `if (!h)` (`:776`) is false, and `service start` reports the board as up — precisely the
    failure mode the `healthIsOurManagedDaemon` gate was written to prevent (see the contract
    comment at `:705–713`). The takeover port-election still runs and the wrapper still exits 3
    on a lost election, so this mis-reports *success* rather than corrupting state; the
    user-facing lie is "✓ up" instead of the intended "✗ no MANAGED daemon for this
    FLEETDECK_HOME" + squatter diagnostic.
  - **Pre-existing, not migration-introduced:** the pre-migration `.mjs` had an untyped `expect`
    param and called an `async` predicate the same un-awaited way — identical runtime. The
    conversion reproduces it byte-for-byte.
  - **Why strict typing is the hero here:** typing the param as it *reads* —
    `expect?: (h: Health) => boolean` — makes `tsc` reject `:775` immediately
    (`Promise<boolean>` is not assignable to `boolean`, i.e. "you're passing an async function
    to a sync-boolean slot"). That is the exact class of bug maximal-strict is meant to catch.
  - **Why I did NOT fix it in this commit:** the `/goal` is a faithful `.mjs`→`.ts` conversion
    with **no silent behavior move**; awaiting the predicate (`if (h && (!expect || await
    expect(h)))`) would change the supervised-start success/refusal outcome for a real
    squatter — a behavior change that must land as its own reviewed fix, not smuggled inside a
    type migration. I therefore typed the param `expect?: (h: Health) => unknown` (`:695`),
    which is honest about the current call (an async predicate's return is treated as an opaque
    truthy token) and keeps `tsc` green without masking the finding. **This log IS the
    hand-off:** the follow-up fix is to make `waitForHealth` await the predicate and type it
    `(h: Health) => boolean | Promise<boolean>`.

- **Why the rest is noise:**
  1. **TS4111 ×6 — `process.env.X` bracket access.** `noPropertyAccessFromIndexSignature` requires
     `process.env['FLEETDECK_HOME']` etc. Pure syntax; no behavior. (FLEETDECK_HOME, FLEETDECK_PORT,
     FLEETDECK_MANAGED, XDG_CONFIG_HOME, FLEETDECK_PROXY_AUTH, FLEETDECK_TRUSTED_ORIGINS.)
  2. **`prefer-nullish-coalescing` on load-bearing `||`.** Three sites intentionally fall an
     **empty string** back to a default (`FLEETDECK_HOME || default`, `XDG_CONFIG_HOME || default`,
     `spawn.reason || 'unknown'`) — `??` would *keep* `''`, changing behavior. Kept `||` with a
     scoped `// eslint-disable-next-line … -- <empty-string reason>`, the sibling idiom
     (ingest/mdns/events/settings/mail/commands). NB: a `x ? x : y` ternary trips the same rule,
     so the disable — not a rewrite — is the correct move.
  3. **`no-control-regex` ×2 — the whole point of the gate.** `ENV_VALUE_UNQUOTABLE`
     (`/[\u0000-\u001f\u0027\u005c]/`) and `EXEC_ARG_UNQUOTABLE` (`/[\u0000-\u001f"]/`) exist to
     *refuse* NUL/C0 controls in env values and ExecStart args. Scoped disable-with-reason.
     (Editor note: these lines carry literal control-range escapes; the disables were inserted via
     `sed` anchored on the const names because the Edit layer decodes `\uXXXX` before matching.)
  4. **`no-base-to-string` — `String(output ?? '')` in `parseTmuxVersion`** (`tmux-version.ts:15`):
     intentional coercion of untrusted `tmux -V` output, matching the `.mjs`. Scoped disable.
  5. **`no-unnecessary-type-conversion` — `Number(h.pid)` kept** (`:665`). `h` is an unchecked
     cast of wire `/health` JSON; `Number()` coerces a stringy pid *before* the integer guard —
     load-bearing for the kill-target identity check. Kept with a disable. (Contrast: dropped a
     provably-redundant `String(s)` in `shQuote`, whose param is a typed `string` path, never wire.)
  6. **Trivia:** `takeoverPidHelpers ??=` memo, one optional-chain (`record?.pid`), removed a
     useless `= null` init, and a comment inside the detached-supervisor `child.once('error', …)`
     empty handler (failures surface via the health probe) — all sibling idioms, no runtime move.

- **Fix:** all in place. Isolated `tsc` clean on `bin/fleetdeck.ts`, `bin/tmux-version.ts`,
  `scripts/fleetd/spawn.ts`; `eslint` exit 0; control-bytes 0. Importer repointed:
  `scripts/fleetd/spawn.ts:36` → value-imports `{ MIN_TMUX_VERSION, tmuxVersionCapability }` from
  `../../bin/tmux-version.ts`; `tests/cli.test.mjs:57–58` → `../bin/fleetdeck.ts` +
  `../bin/tmux-version.ts` (in-process source lane), its `:611` subprocess still runs the built
  `.mjs` artifact; `tests/cli-serve-paths.test.mjs` packs only the self-contained `bin/fleetdeck.mjs`
  (tmux-version inlined, no sibling copy). Suites green: `cli.test.mjs` + `cli-serve-paths.test.mjs`
  → 48 pass / 0 fail. Daemon rebundle from `.ts` for the `spawn.ts` value-import rides task #6.
