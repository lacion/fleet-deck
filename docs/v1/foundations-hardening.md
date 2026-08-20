# Foundations-Hardening — the Bun-primary spine, before the pillars

*Part of [Fleet Deck v1.0](./README.md). This phase runs **after the TypeScript migration completes** and **before any pillar (P1–P7) starts**. It is the outcome of the 2026-08-09 "all cards on the table" review ([[bun-native-stack-decision]]) and it **supersedes [foundations](./foundations.md) F2**: where F2 says Bun is an additive, cuttable distribution win and the core stays Node, this phase makes **Bun the primary runtime**. Where F2 and this doc disagree, this doc wins — and the propagation debt that creates is listed in §8.*

> **Outcomes (updated 2026-08-19 — what actually landed since this plan was written; this note and the linked decision records supersede stale original-step assumptions below).**
> - **§8's one genuinely-open P0 — "does the fail-open floor stay Node, or move to Bun?" — is RESOLVED: it moved to Bun.** `beb18cea` deleted the Node floor; `engines` is now `bun >=1.3.14`; `hooks/hooks.json` execs `bun "…/scripts/fleet-*.mjs"`. There is no longer a second runtime, so §8's open question and the gate's closing clause are settled.
> - **Step 0.5 relocation DONE** — the daemon lives at `src/daemon/`, with the import boundaries both lint- and test-enforced.
> - **Step 1 Biome DONE** (`e1b25f5d` + `826a2f7d`) — Biome 2.5.8 replaced ESLint/Prettier/typescript-eslint; the toolchain gate is now `bun run ci` = `biome ci` (format + lint).
> - **Step 5 landed WITHOUT Hono.** It shipped as `Bun.serve` + native WebSocket with **zero runtime deps** (`dependencies: {}`); Hono was evaluated and **not adopted**. Read every "Hono" mention below — in *What this phase is*, §8, and the one-line gate — as superseded: the gated route/middleware decomposition re-homes onto plain `Bun.serve`, and its pure-unit extraction is now pre-work for Step 7 (Effect).
> - **Step 7 is accepted as an Effect v4-on-Bun application migration.** Fleet Deck explicitly
>   accepts an exact-pinned v4 RC and selected unstable modules. One root Effect runtime will own
>   daemon resources and asynchronous workflows while Bun remains the native platform; pure
>   domain code, shared contracts, board code, and thin fail-open hook shims stay plain. The
>   [adoption decision](./effect-feasibility.md) and [P0–P14 implementation plan](./effect-migration-plan.md)
>   win wherever they disagree with the original Kysely/Hono/Zod/whole-spine assumptions below.
> - **Current transport/build boundary:** the one-shot executor and termbridge still use
>   `node:child_process`, mDNS still uses `node:dgram`, and daemon/bin/hooks still use esbuild with
>   committed `.mjs` artifacts. The suite now has 137 test files. Direct Bun process/UDP/build and
>   compiled-executable work is not retroactively "landed"; each is gated in the new plan.

---

> **Archive boundary.** The sections below preserve the original 2026-08-09 proposal and its
> rationale as an evidence trail; they are not executable instructions. Current status is the
> Outcomes block above, and all remaining work is governed by the linked Effect decision/plan.

## Archived proposal — what this phase was expected to be

F1 (TypeScript, contracts-first) and F2 (Bun as an *optional* binary) were written to be doctrine-neutral: no native deps, `node:sqlite` only, ship one esbuild bundle, keep the fail-open plugin path under Claude's own Node. That doctrine bought exactly one thing — **zero-install fail-open under a known interpreter**. This phase spends it on purpose.

The decision: **go Bun-primary.** That single move unlocks a coherent set of swaps that are individually blocked while we stay Node-agnostic:

- **`bun:sqlite`** replaces `node:sqlite` (and retires the 22.5–22.12 boot floor, [[local-dev-018-testing]]);
- **`Bun.serve` + native WebSocket** replaces the `ws` package — and Bun's WS **pub/sub topics** (`ws.subscribe` / `server.publish`) are the native answer to the P6 delta/per-channel problem ([validation-and-gates](./validation-and-gates.md#5-performance-bars-name-the-budgets));
- **`bun build --compile`** produces one static binary — no esbuild bundle step, no committed `fleetd.bundle.mjs`;
- **`bun test`** runs the suite in-process — the `run-tests-filewise.mjs` Node-runner workaround is deleted.

On top of the runtime swap this phase landed **Biome** and transactional `user_version`
migrations. Kysely, Hono, and Zod were evaluated assumptions, not prerequisites: the production
spine is direct `bun:sqlite`, hardened `Bun.serve` + native WebSocket, and the existing contracts
validators. **Effect v4 is the remaining accepted foundation**, implemented by the separately
gated [migration plan](./effect-migration-plan.md).

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
| 4 | **Direct `bun:sqlite` seam** — LANDED without Kysely | the synchronous centralized query layer stayed smaller and faster; Effect will own lifetime/workflow errors without forcing a SQL abstraction |
| 5 | **`Bun.serve` + native WebSocket** — LANDED without Hono | the hardened native adapter owns auth, CSRF, body drain, upgrades, backpressure, and fail-open behavior |
| 6 | **Existing contracts validators** — LANDED without Zod | shared DTOs stay dependency-light; daemon-internal Effect Schema may be used without pulling Effect into board/hooks |
| 7 | **Effect v4-on-Bun migration** — ACCEPTED, P0–P14 | one root Scope, Bun-native process service, structured boot/schedules, Effect application workflows, scoped HTTP/WS/store/terminal/discovery, and evidence-gated platform trials |

Step 2 is complete. Step 7 deliberately separates the one-shot `Bun.spawn` pilot from the later
tmux stream conversion so the process adapter, Effect runtime, and control protocol are never
debugged as one flag-day change.

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
| **tmux `-C` control-mode under `Bun.spawn`** | the #1 risk — a long-lived, `%`-prefixed, line-buffered control pipe with backpressure, owned by `termbridge.ts`. Node's `child_process` handles it today; direct Bun streams still need their dedicated parity gate. |
| **mDNS multicast under Bun** | `node:dgram` multicast is Bun's **weakest node-compat corner**; the LAN-share path (`mdns`, 1,012 lines) rides it. |
| **native WS pub/sub** | `Bun.serve` WS semantics differ from the `ws` package; the board's reconnect/heartbeat contract (`board/src/useFleetState.ts`) and the coalesced snapshot broadcast (`http` `BROADCAST_COALESCE_MS=60`) must survive the swap. |
| **124/124 green under `bun test`** | the suite is the trust anchor ([[test-suite-is-trust]]). All 124 files green under `bun test` **before** `run-tests-filewise.mjs` is deleted — never the reverse. |

Gate rule: **green-or-replan.** No pillar work starts on an unproven runtime.

---

## 5. The seams Effect actually uses

The accepted 2026-08-19 decision resolves the earlier speculative Zod/Kysely fork:

**A. Shared-schema seam — contracts stay framework-free.** `contracts/` is imported by the board
and hook floor, so it keeps the existing wire types and deliberately loose fail-open validators.
Effect Schema may validate daemon-internal configuration or application inputs, but it never
becomes a transitive browser/hook dependency and never creates a second authority for a public
payload.

**B. Store seam — direct `bun:sqlite` under an Effect service.** The DB remains synchronous and
centralized. A scoped Layer owns open/close and application workflows expose typed Store failures;
pure statements and row mapping stay plain. `@effect/sql-sqlite-bun` is an exact-version,
independently benchmarked option because it changes serialization, busy timeout, WAL, and
transaction behavior—not the assumed default.

**C. Platform seam — Bun capability by capability.** Effect owns lifetime; Fleet Deck keeps or
builds the native adapter whose semantics pass. In particular, the first process service uses
direct `Bun.spawn` because RC.110's platform-bun process adapter re-exports the Node implementation,
while the current hardened `Bun.serve` transport is scoped before any Effect HTTP adapter trial.
See the [decision](./effect-feasibility.md) and capability register in the
[implementation plan](./effect-migration-plan.md#4-bun-native-capability-register).

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
| Effect-owned resources, structured concurrency, schedules, and typed operational policy | Effect v4 is an accepted **RC** dependency — pin the exact cohort, isolate unstable imports, and rehearse atomic upgrades |

---

## 8. Doctrine check — and the propagation debt

**This phase changes doctrine, and that must not be silent.** The [README doctrine](./README.md#the-doctrine-evolved) rules 1–5 (drive-by-default, no core model calls, PR-scoped forge write, loopback, terminal-authoritative) all still hold — they are runtime-agnostic. What changes is the **implicit sixth rule** F2 encoded: *no native deps, one bundle, fail-open under Claude's own Node.*

**The former open question is resolved:** the daemon and fail-open hook floor run on Bun. The hook
shims remain small and Effect-free so a per-event invocation does not pay daemon-runtime startup or
dependency cost. Effect is a daemon application dependency, not a reason to turn every launcher
into an Effect program.

**Propagation debt — docs this phase invalidates (must be rewritten before pillars build on them):**

- **[foundations](./foundations.md) F2 + Doctrine check** — "additive, cuttable, plugin-stays-Node, `node:sqlite × bun:sqlite` matrix" is superseded by Bun-primary;
- **[validation-and-gates](./validation-and-gates.md) §1** F1/F2 row ("plugin stays Node; F2 cut with a note"), **§4** two-runtime CI matrix, **§6** Node-floor / brew platform statement;
- **[architecture](./architecture.md)** runtime/DB seam (F2) — the "one import site + class alias, node↔bun matrix" framing becomes "Bun is the runtime";
- stale `.mjs`/`.js` line-anchors across the folder (e.g. `foundations.md` still cites `board/src/useFleetState.js:25-41`, now `useFleetState.ts`).

Same propagation discipline the folder already owes elsewhere ([[fleetdeck-v1.0-vision]], [[drive-and-observe-provider]]): a decision isn't landed until every doc that asserted the old invariant is corrected.

---

## The gate, in one line

**The remaining foundation gate is:** execute
[effect-migration-plan P0–P14](./effect-migration-plan.md#7-work-package-graph-and-operating-rule)
with exact RC pins, fail-open fixtures first, Bun-native capability gates, source/generated parity,
blocking Linux and macOS lifecycle coverage, no leaked resources, and the propagation docs updated.
Kysely, Hono, Zod, an Effect SQL driver, a Bun UDP adapter, and a compiled executable are not
prerequisites unless their independent evidence gates select them.
