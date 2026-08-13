# Validation & gates — what a credible 1.0 needs beyond the pillars

*Part of [Fleet Deck v1.0](./README.md). The vision dropped the validation section that [fleetdeck-future.md](../fleetdeck-future.md) had; the review demanded it back. This doc is the cross-cutting release-gate checklist: proofs, migrations, tests, perf, platform, security. **Nothing cuts v1.0 until this is green.***

---

## 1. Validation proofs — per pillar

`fleetdeck-future.md` proved every claim four ways: **fail-open**, **determinism**, **exposure**, **version-resilience**. The v1.0 vision dropped that discipline; port it back, per pillar.

| Pillar | Fail-open proof | Determinism / exposure / honesty proof |
|--------|-----------------|----------------------------------------|
| **[P1](./p1-codex-provider.md)** Codex | Codex hooks disabled → card **degrades to notify + pane-liveness, labeled**, never claims parity | Reduced-but-derived card renders only what the strategy supports; Tier C absences labeled, not silently missing |
| **[P2](./p2-harvest-surface.md)** harvest | Checkpoint failure / tripped size-guard **never blocks the Stop hook** | Diff / checkpoint / revert are **git-only, reproducible** from `base_oid` |
| **[P3](./p3-issue-pr-spawning.md)** issue/PR | Forge CLI absent → read/spawn degrades, no crash | **Exposure:** no forge token in `/state`, argv, or logs. **Injection:** an issue body cannot escalate a spawn past *supervised* |
| **[P4](./p4-usage-accounts.md)** usage | File absent/lagging → **"unknown," never a guessed reset** | **Exposure:** no account credential / config-home on `/state`, argv, or logs |
| **[P5](./p5-programmable-fleet.md)** fleet | Dead worker → **synthesized `blocked`**, coordinator never hangs | **Authz:** default-config loopback cannot reach spawn-with-`setup_cmd` without an operator token; agent spawns hit the per-hour cap |
| **[P6](./p6-unified-views.md)** views | Stream/chat is a board page; terminal stays authoritative if it fails | **Doctrine:** no board surface is load-bearing. **Perf:** stream + WS broadcast within budget at 15 sessions |
| **[P7](./p7-drive-and-observe.md)** drive | Runner / SDK / login down → session **falls back to the observed floor**, full card, fleet stays lit | **Determinism:** driven cards stay pure derivation — the same hooks fire under the SDK (the linchpin), so the daemon never parses a model response to render. **Exposure:** no subscription/OAuth credential in `/state`, argv, or logs; the driver child is confined to its worktree |
| **[F1/F2](./foundations.md)** | Plugin stays Node (fail-open path untouched); F2 additive only | F1: **`tsc --noEmit` green + runtime boundary validation**. F2: **node×bun CI matrix green**, or F2 cut with a note |

---

## 2. Upgrade / migration story (the review's biggest omission)

Today there are **~30 ad-hoc `ALTER TABLE` migrations** implemented as column-introspection branches with **no `user_version` and no transaction** around the migration block (`db.mjs:261-386`). Workable at today's churn; **not** at v1.0's, which adds **≥4 new tables/columns**: base ref (P2), checkpoint refs (P2), completions (P5), event stream (P6), accounts (P4).

**Adopt for 1.0:**

- **numbered, transactional migrations** gated on **`PRAGMA user_version`** — the migration block runs inside a transaction and bumps the version atomically;
- a stated **compatibility rule per skew direction** — *old daemon + new board?* *new hooks + old daemon?* (the SessionStart shim already prefers a committed bundle, which helps);
- a **downgrade answer** — what a rolled-back daemon does when it meets a newer schema.

Detail also in [architecture](./architecture.md) §Migrations & versioning.

---

## 3. Versioning of hook payloads & the control API

Carry a **`schema_version`** in **canonical events** *and* in the **daemon-served control skill** (P5), so agents and hooks **detect skew** instead of silently misbehaving — the run-nonce bug was exactly this class. Cross-link [architecture](./architecture.md) (canonical vocabulary) and [P5](./p5-programmable-fleet.md) (served skill).

---

## 4. Test strategy — named per pillar

The suite is the repo's **trust anchor** — [[test-suite-is-trust]]: never quarantine a failing test to make a migration "pass." Every pillar names its new test assets up front.

| Pillar | New test assets |
|--------|-----------------|
| **F1/F2** | Two-runtime CI (`node:sqlite` × `bun:sqlite`); a required **`tsc --noEmit`** lane |
| **P1** | **Hook-shape fixtures per provider version** (pin Codex + Claude payloads; upstream churn becomes a fixture diff, not a state-machine surprise) |
| **P2** | **Git-fixture repos** for checkpoints/diff: base-ref stamping, revert, passing-Stop dedupe, size-guard degradation |
| **P3** | **Injection fixtures** (hostile issue body → spawn stays supervised); forge-CLI mocks; a **no-token-in-`/state`** assertion |
| **P4** | **Unknown-state fixtures** (`rate_limits: null`); a **rollout-tail** test that proves we never slurp |
| **P5** | **Authz tests** (tokenless spawn-with-`setup_cmd` rejected; per-hour spawn cap); dead-worker synthesis; idempotent ack |
| **P6** | Stream write-volume test; WS broadcast-pressure test |
| **P7** | **Linchpin fixture** (an SDK-driven `query()` still POSTs the `http` hooks → identical canonical stream); **floor-fallback** test (kill the runner mid-turn → card degrades to the floor, no dark); a `canUseTool` approval round-trip; interrupt/steer land within the turn |

---

## 5. Performance bars (name the budgets)

- **15 sessions × per-turn checkpoints** — a real git cost budget. Checkpoints run **async off the hook path** with a size guard, and must add **zero** hook-response latency (P2).
- **Event-stream write volume** — **selective** tool events, not a firehose; PostToolUse does not tick today and a full firehose would swamp the table and the broadcast (P6).
- **WS full-snapshot broadcast pressure** — `BROADCAST_COALESCE_MS=60` (`http.mjs:1370-1382`); the stream needs a **delta or per-channel fetch**, not the broadcast-everything path (P6).
- **Rollout tails** — Codex files balloon (20 MB+ observed); **tail, never slurp** (P1, P4).

---

## 6. Platform statement (promises the CI must back)

- **Codex hooks are Windows-disabled** — a Codex card on Windows is **notify + pane-liveness only**, and must say so (P1).
- **macOS CI is advisory-only today** ([[oss-repo-infrastructure]], issue #2) — a **brew-distributed binary (F2) implies promises the CI does not currently back**. Either strengthen the macOS lane or scope the brew claim down.
- **Node floor** — `node:sqlite` on 22.5–22.12 can't boot the daemon ([[local-dev-018-testing]]); the standalone Bun binary is *partly a fix* for this (F2), which is another reason F2 is a distribution win, not just packaging.

---

## 7. docs/internals (the t3code lesson the vision dropped)

A **glossary + route map + state-machine doc** is near-free and 1.0 is its natural moment. Name it a deliverable: the **canonical event vocabulary**, the **provider strategy surface**, the **route table**, and the **lifecycle state machine** — the internals a new contributor needs to touch the spine without re-deriving it from `events.mjs`.

---

## 8. The security-review gate (before the cut)

A **delta audit of the new surface only** ([[fleetdeck-security-standing]] — audit only deltas since v0.22.x; accepted trust-zone residuals stand):

- **forge writes** (P3) — verb allowlist honored, per-write confirm + audit line, **no token leak** into `/state`/argv/logs;
- **the control API** (P5) — token classes enforced, spawn caps active, the tokenless spawn-with-`setup_cmd` finding closed;
- **multi-account env** (P4) — config-home per session, **no credential** on `/state`/argv/logs;
- **the drive-control surface** (P7) — approve / interrupt / steer / resume are **operator-gated** like the terminal route; the SDK / app-server child is confined to its session worktree and cannot be steered by a worker token; **no subscription/OAuth credential** leaks into `/state`/argv/logs.

This is **step 8 of the [README sequencing](./README.md#sequencing-to-10-revised)** — the cut is gated on it.

---

## The gate, in one line

**No v1.0 cut until:** every pillar's proofs pass; migrations are numbered + transactional under `PRAGMA user_version`; the per-pillar test assets exist and the suite is green on **both** runtimes; the perf budgets hold at 15 sessions; the platform statement matches what CI actually backs; every driven session provably **falls back to the observed floor** when its runner drops; and the security **delta** audit is clean (including the drive-control surface).
