<!-- STAGING for http.ts — merge into ts-migration-bugs.md (newest-first, right below the
     "entries appended below" marker) once the concurrent board subagent has finished its own
     append. Kept separate only to avoid a concurrent-append race on the shared log. -->

### http.ts — the HTTP + WebSocket surface (~1960 loc): all NOISE, one LOAD-BEARING near-miss avoided, every security invariant intact   [NOISE]
- **What:** the daemon's whole request surface — the `authorized()` gate, the CSRF wall
  (`crossSiteReason`), DNS-rebinding `hostHeaderOk`, the PROXY_AUTH trust semantics, gateway-bearer
  waivers, `serveBoardAsset` traversal safety, both WS upgrades (`/ws` state feed, `/ws/term`
  terminal bridge) with their backpressure/keepalive caps, and the hook fail-open contract.
  `tsc` (root maximal-strict) + type-aware `eslint` driven to **0**, control-bytes 0,
  **zero runtime move**. Every finding was maximal-strict ergonomics or a lint idiom already
  established by sibling `spawns.ts`/`derive.ts`/`settings.ts`. The security-critical surfaces were
  preserved verbatim: the unconditional `/hook/*` auth leading the loopback block; timing-safe
  `tokenMatches` only after a length match; the CSRF wall on state-changing POSTs, both WS upgrades
  and the two mutating GETs (`/mail`, `/api/watch`); `hostHeaderOk` rebinding defense; the
  no-Origin proxy-hole fix (`arrivedViaTrustedProxy` checks Host, not just Origin); the gateway
  bearer gate keyed off `isLoopbackAddress(req.socket.remoteAddress)`; `serveBoardAsset` staying
  strictly inside `BOARD_DIST`; CSP_SHELL on HTML only; the WS eviction caps
  (`MAX_WS_BUFFER`/`MAX_TERM_WS_BUFFER`/`MAX_TERM_FRAME_BYTES`); `spawnFailureReason` redaction on
  the `/api/spawn` catch; and hooks always failing open with `200 {}`.

- **The one that mattered — a LOAD-BEARING `|| null` that `??` would have silently broken (near-miss, NOT shipped):**
  `prefer-nullish-coalescing` flagged the `/api/watch` watch-generation read
  `const wg = url.searchParams.get('wg') || null`. A naive `?? null` autofix looks identical but is
  **not**: `searchParams.get()` returns `'' | string | null`, and the empty string is reachable
  (`?wg=`). Downstream, `claimMail(sid, gen)` (mail.ts:620) refuses to claim when `gen !== null &&
  !isWatchGen(sid, gen)` — so `''` is a *present-but-invalid* generation that **blocks mail
  delivery**, whereas `null` means "no generation, claim freely." `|| null` folds `''`→`null`
  (deliver); `?? null` would keep `''` (refuse) — a real mail-delivery regression on any client that
  sends `?wg=`. Preserved the behavior without a disable and without the rule's suggested
  behavior-changing shape: `const wgParam = url.searchParams.get('wg'); const wg = wgParam === ''
  ? null : wgParam;`. (Note: the rule *also* flags the ternary `x ? x : y` form and offers the same
  unsound `x ?? y` autofix — do not take it.) Verified against mail.ts:620 before editing.
  watch-rewake (18/18), mail-and-blocking (9/9), mail-delivery-lease (5/5), mail-frames (9/9) green.

- **Why the rest is noise (the interesting ones):**
  1. **CFA over-narrowed a callback-mutated `let` to `false`, tripping `no-unnecessary-condition`
     "always falsy".** The `/ws/term` bridge kept `let socketClosed = false`, set it in a
     `ws.on('close')` closure, and re-read it after an `await` (the R5 abort check). TS narrowed the
     bare `let` to the literal `false` at the read and declared the guard dead. The honest fix is a
     **holder object** — `const abort = { closed: false }`, mutate `abort.closed = true` in the close
     handler, read `abort.closed` after the await — because a property read across an `await` re-widens
     to the declared `boolean` (a function boundary defeats the literal narrowing). No disable, no
     behavior move; `isAborted: () => abort.closed` still fires and a late-arriving handle still closes.
  2. **`no-misused-promises` on an async WS `connection` listener.** `termWss.on('connection', async
     (ws, req) => {…})` handed a promise-returning listener to an EventEmitter. The body is fully
     try/caught and never rejects, so the sanctioned fix (same as spawns.ts's `setTimeout(async)`
     nudge) is a sync listener wrapping the body in `void (async () => { … })();`. The file was
     briefly unbalanced between the open and close edits — PostToolUse prettier skips a
     syntactically-broken file and reindented once the closing edit balanced it.
  3. **`req.socket.remoteAddress` optional-chain was flagged unnecessary.** `@types/node` types
     `IncomingMessage.socket` as a non-nullable `Socket`, so `req.socket?.remoteAddress` trips
     `no-unnecessary-condition`. Dropped the `?.` at all three sites (the loopback gate, the LAN
     log `from`, and the `bearerWaived` chain) — the gateway bearer gate still keys off
     `isLoopbackAddress(req.socket.remoteAddress)` exactly as before.
  4. **`e?.reason || e?.message` on a WS error is first-non-empty, NOT nullish.** The `/ws/term`
     error branch surfaces the closer reason, then the message, then a default. `??` would keep an
     empty-string `reason`. Preserved with string-normalization:
     `const failReason = typeof e?.reason === 'string' ? e.reason : ''; const failMessage = …;
     send({ t: 'err', reason: failReason || failMessage || 'terminal unavailable' });` — the
     remaining `||` chain is now over provably-string operands where empty-means-fall-through is the
     intended semantics. terminal-ws (18/18) green.
  5. **`no-base-to-string` on `unknown` spawn-plan id.** The spawn-accept log builds a ` plan=${…}`
     suffix from a value typed `unknown`. Guarded to a stringifiable primitive rather than coerce a
     possible object: `const spawnPlanSuffix = typeof spawnPlanId === 'string' || typeof spawnPlanId
     === 'number' ? \` plan=${spawnPlanId}\` : '';`.
  6. **`String(x)` on an already-`string` and `String(x || '')` folds.** Removed the redundant
     `String()` wrappers and simplified empty-string folds that are identical either way
     (`url.searchParams.get('session') ?? ''` for the `/mail` and watch `sid` keys, `req.url ?? ''`
     in `.startsWith('/hook/')` and `new URL(req.url ?? '/', …)` contexts) to the established `??`
     idiom — all behavior-identical because the consumer only ever does string ops.
  7. **Assorted maximal-strict ergonomics (pure NOISE, no behavior):** `use-unknown-in-catch-
     callback-variable` → `.catch((err: unknown) => …)` on the worktree-inspector / session-fs /
     home-fs detach paths; `no-unused-expressions` ternary-as-statement → `if (…) json(…); else
     json(…);` at the hook-vs-403 forks (`hostHeaderOk`, `crossSiteReason`, request-error catch);
     `no-empty-function` → explanatory-comment bodies on the initial no-op `unregister` and
     best-effort `.catch()`s; `timer.unref()` / `flushTimer.unref()` retained; the fire-and-forget
     `spawnLivenessTick()` cast to `Promise<unknown> | undefined` before `?.catch()` (the ctx seam
     types it loosely — NOISE, the tick is reconcile-only and its rejection is intentionally
     swallowed).

- **Seam notes (NOISE, no fix needed, flagged for the derive/ctx typing pass):** the `ControlResult`
  envelope, `LiveSocket.isAlive`, and the `{ raw: Buffer }` WS-frame assumption are all typed
  loosely at the `CoreCtx`/ws boundary; http.ts consumes them defensively and unchanged. No latent
  bug — recording only so the eventual ctx-contract tightening knows these three are the remaining
  loose edges the HTTP surface leans on.

- **Fix:** all in place; no runtime behavior moved. Rename staged `http.mjs -> http.ts`; importers
  repointed to `.ts` (`scripts/fleetd/fleetd.mjs:19`, `tests/lan-mdns-state.test.mjs:15`,
  `tests/network-refresh.test.mjs:24`, the generated wrapper in `tests/hook-auth.test.mjs:104`, the
  source-text read in `tests/board-util.test.mjs:617`, and the ESM loader specifier in
  `tests/helpers/mdns-dgram-loader.mjs:17`). Affected suites green on the source lane:
  lan-mdns-state 2/2, network-refresh 3/3, fleetd-audit-regressions 10/10 (loader repoint),
  hook-auth 11/11 (generated import), csrf-guard 42/42, loopback-gates 14/14, require-token 7/7,
  lan-auth 9/9, ws-hardening 5/5, static-serving 9/9, gateway 19/19, mail-and-blocking 9/9,
  mail-delivery-lease 5/5, mail-frames 9/9, takeover 15/15, terminal-ws 18/18, question-rearm 8/8,
  watch-rewake 18/18. (board-util / board-termdiag can only run once the concurrent board
  conversion lands — they top-level-import board sources that are mid-rename to `.ts`; the http.ts
  cache-control invariant they assert is present at http.ts:217 and both files are self-consistent
  at this commit's tree.)

- **Bundle deliberately NOT regenerated in this commit:** `bun run bundle` inlines *every* daemon
  source — including `scripts/fleetd/derive.ts`, which is the concurrent board session's uncommitted
  WIP — into `fleetd.bundle.mjs`. Rebundling now would bake that WIP into a committed artifact.
  Because http.ts is behavior-identical to the old http.mjs (zero runtime move), the shipped bundle
  stays behaviorally correct; the regeneration rides the fleetd.mjs-entry conversion once the board
  WIP has landed.
