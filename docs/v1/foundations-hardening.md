# Foundations-Hardening — the Bun-primary spine, before the pillars

*Part of [Fleet Deck v1.0](./README.md). This phase runs **after the TypeScript migration completes** and **before any pillar (P1–P7) starts**. It is the outcome of the 2026-08-09 "all cards on the table" review ([[bun-native-stack-decision]]) and it **supersedes [foundations](./foundations.md) F2**: where F2 says Bun is an additive, cuttable distribution win and the core stays Node, this phase makes **Bun the primary runtime**. Where F2 and this doc disagree, this doc wins — and the propagation debt that creates is listed in §8.*

---

## What this phase is

F1 (TypeScript, contracts-first) and F2 (Bun as an *optional* binary) were written to be doctrine-neutral: no native deps, `node:sqlite` only, ship one esbuild bundle, keep the fail-open plugin path under Claude's own Node. That doctrine bought exactly one thing — **zero-install fail-open under a known interpreter**. This phase spends it on purpose.

The decision: **go Bun-primary.** That single move unlocks a coherent set of swaps that are individually blocked while we stay Node-agnostic:

- **`bun:sqlite`** replaces `node:sqlite` (and retires the 22.5–22.12 boot floor, [[local-dev-018-testing]]);
- **`Bun.serve` + native WebSocket** replaces the `ws` package — and Bun's WS **pub/sub topics** (`ws.subscribe` / `server.publish`) are the native answer to the P6 delta/per-channel problem ([validation-and-gates](./validation-and-gates.md#5-performance-bars-name-the-budgets));
- **`bun build --compile`** produces one static binary — no esbuild bundle step, no committed `fleetd.bundle.mjs`;
- **`bun test`** runs the suite in-process — the `run-tests-filewise.mjs` Node-runner workaround is deleted.

On top of the runtime swap this phase also lands **Biome**, **transactional `user_version` migrations**, **Kysely** as the typed query layer, **Hono** for the gated route/middleware layer, **Zod v4** at the wire boundary, and **Effect v4** through the daemon spine.

This is a *core runtime swap* — the exact thing F2 said it would never be. That is the point, and the cost is named in §8.

---

## Precondition — a 100% TypeScript base

**No step below begins until the daemon source is fully TypeScript.** Every step here is then a **TS→TS transformation**, never a language change entangled with an architecture change. Entangling the two — rewriting a `.mjs` file *directly* into its Hono/Zod/Effect form — is precisely where an agent-driven refactor drifts silently, because there is no typed intermediate for the compiler or the suite to police.

Current holdouts (verified 2026-08-09 on `fd/typescript-migration`):

| File | Disposition |
|------|-------------|
| `scripts/fleetd/http.mjs` (1,492 lines) | **Convert → `http.ts` plain first**, prove green, *then* rewrite with Hono (step 5). The plain conversion is not wasted — it is what makes the Hono rewrite a typed refactor. |
| `scripts/fleetd/fleetd.mjs` | Convert → `fleetd.ts` (daemon entrypoint). |
| `scripts/fleetd/fleetd.bundle.mjs` | **Generated** (esbuild output) — not source. Disappears under `bun build --compile`; do not convert. |
| `scripts/run-tests-filewise.mjs` | **Deleted** in step 2, not converted (its Node 22.x IPC-bug reason is gone under Bun). |
| `scripts/fleet-hook.mjs`, `fleet-sessionstart.mjs`, `fleet-watch.mjs` | The **fail-open plugin floor** (zero-dep, launched under the hook's interpreter). Whether these join the TS base or stay minimal `.mjs` is tied to the open question in §8 (does the floor stay Node?). Decide there, not here. |
| `tests/**/*.test.mjs` (124 files) | Run **as-is** under `bun test` (node:test in-process); they do not block the base. A `node:assert → expect` codemod for bun-native coverage is a later, optional pass ([[bun-native-stack-decision]] #8). |

`spawn.mjs` is already `spawn.ts` (commit `0446d2d7`), so the strict-Zod step lands on typed code. `statements.ts`, `derive.ts`, `events.ts`, `mdns` are already converted.

---

## The ordered steps

Each layer is made stable before the next one wraps it. **Effect is last — not for effort reasons, but because it wraps every layer beneath it.** You Effect-ify a settled spine, never a moving one.

| # | Step | Why here |
|---|------|----------|
| 0 | **Finish the TS migration** — `http.mjs`→`.ts`, `fleetd.mjs`→`.ts`; suite green | the phase's hard precondition (above) |
| 0.5 | **Relocate + set boundaries** — `git mv scripts/fleetd → src/`; declare the import-boundary rules (below); decomposition **deferred** into steps 4–5 | pure `git mv` once step 0 is committed; boundaries lint-enforced before Kysely/Hono/Effect wrap anything |
| 1 | **Biome** — replace ESLint/Prettier/typescript-eslint | ground-clearing, mechanical, reversible |
| 2 | **Bun-primary spike — THE GATE** (§4) | prove the four risks + `bun test` green before anything depends on Bun |
| 3 | **`user_version` transactional migrations** | independent latent-bug fix ([validation-and-gates](./validation-and-gates.md#2-upgrade--migration-story-the-reviews-biggest-omission) §2); do early, before v1 tables land |
| 4 | **Kysely** on `kysely-bun-worker`, statement-by-statement behind the `sqlite.ts` seam | needs `bun:sqlite` proven (step 2); the 192 SQL sites in `statements.ts` migrate one at a time |
| 5 | **`Bun.serve` + Hono** — rewrite the gated route/middleware layer | needs Bun proven (step 2); Bun's native router has **no middleware** (oven-sh/bun#17608) and `http.ts` is dominated by cross-cutting gates on the RCE-adjacent `/api/spawn` surface |
| 6 | **Zod v4** at `contracts/` — strict `validateSpawnRequest` at Phase 5 | can overlap 4–5; keeps the fail-open validators loose (§6) |
| 7 | **Effect v4** — thread through the now-stable runtime/DB/HTTP spine | Bun + Kysely + Hono settled; the tmux control-mode pipe becomes an Effect `Stream`; fail-open becomes a typed error the HTTP layer still answers `200` on |

**Do not run step 7 concurrently with step 2.** Debugging the fiber runtime, `Bun.spawn`, and tmux control-mode simultaneously is the trap this ordering exists to avoid.

---

## Step 0.5 — source layout: relocate, decompose, enforce boundaries

Step 0.5 sits between "finish TS" (0) and Biome (1): the source is fully typed but not yet reshaped. It is **two different kinds of work with different risk profiles**, and they must never land as one commit.

**Relocation — mechanical, wholesale, one sweep.** `scripts/fleetd/` is where the daemon lived when this was a script; it is the daemon now. Move it to `src/daemon/` as a pure `git mv` sweep — no edits to file *bodies* beyond the import-path rewrites the move forces, suite green before and after, one commit. Because it is `git mv` only it is safe to do wholesale — but only **after step 0 is fully committed** (see the hazard below).

**Decomposition — behavioral-adjacent, deferred, suite-gated.** Breaking the big files down (`http.ts` ~1,492 lines, `mdns` ~1,012, the SQL layer) is *not* a big-bang and *not* part of the relocation commit. A file split moves behavior across module boundaries; done blind it silently drops a code path. So decomposition **rides the steps that already rewrite those files**:

- the `http.ts` split happens *with* the Hono step (5) — lift the cross-cutting gates (loopback/token/CSRF/provenance) and the coalesced broadcaster out into their own units as Hono middleware, along the way;
- the SQL split happens *with* Kysely (4) — `statements.ts`'s 192 sites already migrate one-at-a-time behind the `sqlite.ts` seam; that migration *is* the decomposition;
- packet encode/decode splits out of `mdns` when the Bun spike (2) proves the dgram path.

The lever throughout is the **pure-core / I/O-shell seam**: pull the pure logic (gate decisions, packet framing, snapshot coalescing, SQL building) out of the I/O it is currently married to, so each pure unit is testable without spawning tmux, opening a socket, or touching the DB. That is the "break things down for testing" win — a *consequence* of splitting on that seam, not a separate refactor to schedule.

**The boundary rules (declared at 0.5, enforced from here on).** Four zones, one legal import direction:

| Zone | May import | Must NOT import |
|------|-----------|-----------------|
| `contracts/` | Zod v4 only (nothing heavy) | `effect`, `kysely`, `bun:*`, anything daemon-internal |
| `src/daemon/` | `contracts/`, `effect`, `kysely`, `hono`, `bun:*` | `board/**` |
| `src/plugin/` (fail-open floor) | `contracts/` types only | `effect`, `kysely`, `hono`, any heavy dep — it stays a zero-dep floor (§8 open question notwithstanding) |
| `board/` | `contracts/` **only** | `effect`, `kysely`, anything under `src/` |

`contracts/` is the **single cross-zone import** — the board and the daemon meet there and nowhere else. This is the §5A Schema seam and the §6 board tripwire promoted from per-file caution to a structural rule: *the board never imports `effect`.*

Enforce it the cheap way first — a lint/dependency rule (dependency-cruiser, or Biome's import restrictions) that fails CI on an illegal edge. Graduate to a **Bun workspace** (a `package.json` per zone) only if a boundary earns structural teeth; don't pay the workspace tax up front.

**Hazard — this is a huge `git mv` on a shared branch.** A peer is actively converting daemon files on `fd/typescript-migration` right now. A relocation sweep rewrites nearly every daemon import path; run concurrently with an in-flight conversion it collides head-on. Step 0.5 begins only when **step 0 is done and committed** — the TS migration fully landed, the tree clean — never interleaved with it. This is already the ordering; it is restated because relocation is the step most likely to be rushed into a dirty tree.

---

## 4. The Bun spike is the gate

Step 2 is not a migration — it is a **go/no-go proof**. If it fails, the whole phase re-plans (or F2's "additive, cuttable" stance stands after all). Four things must be green, in a real soak, not a smoke test:

| Proof | Why it's the risk |
|-------|-------------------|
| **tmux `-CC` control-mode under `Bun.spawn`** | the #1 risk — a long-lived, `%`-prefixed, line-buffered control pipe with backpressure, driven from `exec.ts`. Node's `child_process` handles it today; Bun's is unproven for us. |
| **mDNS multicast under Bun** | `node:dgram` multicast is Bun's **weakest node-compat corner**; the LAN-share path (`mdns`, 1,012 lines) rides it. |
| **native WS pub/sub** | `Bun.serve` WS semantics differ from the `ws` package; the board's reconnect/heartbeat contract (`board/src/useFleetState.ts`) and the coalesced snapshot broadcast (`http` `BROADCAST_COALESCE_MS=60`) must survive the swap. |
| **124/124 green under `bun test`** | the suite is the trust anchor ([[test-suite-is-trust]]). All 124 files green under `bun test` **before** `run-tests-filewise.mjs` is deleted — never the reverse. |

Gate rule: **green-or-replan.** No pillar work starts on an unproven runtime.

---

## 5. Two seams Effect forces

Adopting Effect v4 ([[bun-native-stack-decision]] #9 — v4 beta, launched 2026-02-18, `Effect+Stream+Schema` ~20 KB tree-shaken, Standard-Schema Schema) forces two decisions that aren't obvious from "adopt Effect":

**A. Schema seam — Zod at the browser boundary, Effect Schema daemon-side.** `contracts/` is imported by the **board** (`useFleetState.ts` pulls `Snapshot`/`SpawnCapability`/`Lan` from `../../contracts/index.ts`) and is deliberately *not* load-bearing — the board trusts the daemon's wire shape and casts rather than validates (`useFleetState.ts:145-146`, `:191`). Effect Schema tree-shakes and is Standard-Schema-compliant, but it is **beta with a churning API**, and beta churn must not sit on the browser-shared wire contract. So:

- **Zod v4** stays at `contracts/` — stable, Standard-Schema, board-safe;
- **Effect Schema** is daemon-internal only;
- both implement Standard Schema, so a daemon-side Effect Schema consumes a Zod-validated wire object with zero friction, and the board **never imports `effect`**.

**B. DB fork — Kysely wrapped in Effect now; `@effect/sql` later.** If Effect owns the spine, a bare Kysely call is a Promise you wrap in `Effect.tryPromise`. The idiomatic alternative is `@effect/sql` + a `bun:sqlite` dialect (DB ops as Effects: typed errors, managed connections, transactions-as-Effects) — but that stacks a *second* beta on the data layer. Decision: **keep Kysely (`kysely-bun-worker`) wrapped in a thin Effect layer at the `sqlite.ts` seam** — one beta, mature SQL, and the wrapping lives behind the seam we're already migrating statement-by-statement. Revisit `@effect/sql` once Effect v4 goes stable.

---

## 6. The agent-loop discipline (fail-open is the thing at risk)

The mechanical conversions here are agent-loop work. An agent-driven **whole-spine rewrite is the single highest-risk place for silent behavior drift**, because Effect changes control flow — error propagation, concurrency, and the daemon's **fail-open** posture. The risk is not that the agent breaks something loudly; it is that it makes the daemon *more correct-looking and less fail-open* — rejecting where today it passes through.

The two hard rules:

- **Pin the fail-open behavior with fixtures *before* the Effect pass.** The loose hook validator gates only a blank `session_id` and passes everything else through, and the endpoint answers `200 {}` even on a rejected body — with the standing contract *"wiring it in later must not change a single test outcome"* (`contracts/hooks.ts:1-11`, `:71-84`). That behavior, the `200`-on-reject path, and the reconcile-never-hangs paths become tripwire fixtures first, so any drift toward strictness fails a test.
- **The loop gates green-or-revert, never quarantine** ([[test-suite-is-trust]]). An agent that edits a test to make its refactor pass is the exact failure mode this discipline exists to catch.

**Board tripwire:** if the loop ever pulls `effect` into a `board/` import, the daemon/board boundary (§5A) has been crossed — treat it as a red flag, not a convenience.

---

## 7. What we gain and what we pay

| Gained | Paid |
|--------|------|
| `bun:sqlite` (no `node:sqlite` version floor) | per-platform binaries; **macOS CI becomes load-bearing**, not advisory ([[oss-repo-infrastructure]] issue #2, [validation-and-gates](./validation-and-gates.md#6-platform-statement-promises-the-ci-must-back) §6) |
| native WS + pub/sub (drop `ws`; P6 deltas for free) | Bun's dgram/WS/control-pipe corners are now on the critical path (§4) |
| `--compile` single binary (drop esbuild + committed bundle) | the build is a Bun build, not a portable-`.mjs` build |
| `bun test` (drop the filewise runner) | `bun test` lacks coverage/reporters/snapshots/`mock.module` today |
| Kysely typing + Hono middleware + Effect error model | Effect v4 is **beta** — pin the exact `4.0.0-beta.N`, expect to re-run the loop on each bump (`ServiceMap → Context` and Schema renames already landed mid-beta) |

---

## 8. Doctrine check — and the propagation debt

**This phase changes doctrine, and that must not be silent.** The [README doctrine](./README.md#the-doctrine-evolved) rules 1–5 (drive-by-default, no core model calls, PR-scoped forge write, loopback, terminal-authoritative) all still hold — they are runtime-agnostic. What changes is the **implicit sixth rule** F2 encoded: *no native deps, one bundle, fail-open under Claude's own Node.*

**The one genuinely open question this phase must answer: does the fail-open plugin floor stay Node, or move to Bun?**

- If it **stays Node**, the plugin-embedded daemon keeps `node:sqlite` + `ws` and we maintain *two* runtimes — undercutting "Bun-primary."
- If it **moves to Bun**, Bun must be present where the SessionStart hook launches the daemon — spending the "zero-install fail-open under a known interpreter" the doctrine was buying. (Supporting data point, not a resolution: Anthropic already ships the Claude Code CLI on Bun, so for Claude-plugin users the hook's interpreter increasingly *is* Bun — but this cannot be assumed for every install.)

This is a P0 decision for the phase; it is not resolved in this doc.

**Propagation debt — docs this phase invalidates (must be rewritten before pillars build on them):**

- **[foundations](./foundations.md) F2 + Doctrine check** — "additive, cuttable, plugin-stays-Node, `node:sqlite × bun:sqlite` matrix" is superseded by Bun-primary;
- **[validation-and-gates](./validation-and-gates.md) §1** F1/F2 row ("plugin stays Node; F2 cut with a note"), **§4** two-runtime CI matrix, **§6** Node-floor / brew platform statement;
- **[architecture](./architecture.md)** runtime/DB seam (F2) — the "one import site + class alias, node↔bun matrix" framing becomes "Bun is the runtime";
- stale `.mjs`/`.js` line-anchors across the folder (e.g. `foundations.md` still cites `board/src/useFleetState.js:25-41`, now `useFleetState.ts`).

Same propagation discipline the folder already owes elsewhere ([[fleetdeck-v1.0-vision]], [[drive-and-observe-provider]]): a decision isn't landed until every doc that asserted the old invariant is corrected.

---

## The gate, in one line

**No pillar (P1–P7) starts until:** the source is 100% TS; the daemon is relocated to `src/daemon/` with the import boundaries lint-enforced; Biome is in; the Bun spike is green on all four proofs and 124/124 under `bun test`; migrations are transactional under `user_version`; Kysely, Hono, and Zod-at-`contracts/` have landed with the suite green at every step; Effect v4 wraps the stable spine with fail-open pinned by fixtures first; and the F2/validation/architecture propagation debt (§8) is paid — including an answer to *does the fail-open floor stay Node.*
