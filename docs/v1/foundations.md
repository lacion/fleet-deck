# Foundations — the platform bets everything else rides on

*Part of [Fleet Deck v1.0](./README.md). See [architecture](./architecture.md) for the contracts module and the DB seam these two foundations feed. Where the vision and review differ, the review wins.*

Two questions to settle before the pillars land, because the pillars are heavier if the ground is soft. **F1** decides whether the growing multi-provider surface is held together by typed contracts or by comments and hope. **F2** decides how the standalone board ships without dragging the Node-version dance behind it. Neither is a rewrite; both are seams. Get them wrong and every pillar pays interest.

---

## F1 — TypeScript, contracts-first

The vision undersold this badly. It called the daemon "~200 KB of plain `.mjs`." The tree says otherwise: **18,250 lines across 35 modules (~1 MB source) + a committed 630 KB bundle (`package.json:44`)**, zero TS, zero `@typedef` payload shapes. The `/state` contract is **hand-mirrored** in `board/src/useFleetState.js:25-41` with the rules living in **comments** on both sides of the wire.

So the *motivation* is **stronger** than the vision claimed — the contract is literally prose today — but the *size* was **~5× understated**. That flips the verdict: "migrate the daemon, contracts-first, before the surface grows" as written is a **boil-the-ocean invitation**, not a foundation. The fix is to split it.

### F1a — the contracts module (do first, timebox ~1–2 weeks)

A `contracts/` module (T3 Code's `packages/contracts` model) holding the typed shapes for every boundary, consumed by the daemon **and** the board (**and** the future Bun binary):

- the `/state` snapshot (kills the comment-contract in `board/src/useFleetState.js:25-41`);
- the **canonical hook events** — see [architecture](./architecture.md#layer-2--canonical-event-vocabulary);
- the `/api/spawn` body, and the mail/command/questions wire formats;
- a **`schema_version`** field on every shape **from day one**, so hooks, board, and daemon detect skew (the run-nonce work was exactly this class of bug).

**F1a and the P1 intake-normalization are the same task.** The canonical event vocabulary *is* the contracts-first work; the P1 provider seam *is* the first consumer of it. Sequence them as one unit — see [architecture](./architecture.md#the-one-decision-that-matters-normalize-at-intake-not-in-derive).

Two requirements the Codex pass landed, both non-negotiable:

- **A type-checker must exist.** `esbuild` **strips types without checking them**, and **no `tsc` runs anywhere in the repo today** — so types would be decorative. F1 adds a required CI lane: `tsc --noEmit` (or `checkJs` on the not-yet-converted `.mjs`).
- **Static types do not validate wire input.** Hook bodies and spawn bodies are hostile, attacker-reachable boundaries. F1a ships **runtime validation** of that JSON at intake (reject malformed, fail-open) alongside the static shapes. Types guard *our* code; the validator guards the *wire*.

### F1b — the standing rule (not a phase)

- All **new** v1.0 code is TS against the contracts.
- Existing modules convert **only when a pillar touches them** — P1 naturally converts `events`/`derive`/`ingest`; P2 converts `worktrees`. No big-bang conversion.
- Tests stay green throughout ([[test-suite-is-trust]] — no quarantining to make a migration "pass").

### Missing pieces to nail down

- a **`tsconfig`** strategy (strictness, `allowJs`/`checkJs` for the transition, module resolution matching esbuild);
- **board typing** — `.jsx → .tsx` opportunistically as components are touched, not up front;
- the **`schema_version`** field, restated because it is the cheapest insurance in the whole plan.

**Verdict:** F1 is a foundation, not a rewrite. F1a is a bounded 1–2 week module; F1b is a rule that costs nothing and pays out on every pillar.

> **Companion playbook:** the concrete, file-by-file mechanics — how to move all 34 daemon modules from `.mjs` to `.ts` one at a time with JS and TS running side by side, the per-file recipe, the data-driven conversion order, and the restructure-as-you-convert layout — live in **[ts-migration](./ts-migration.md)**.

---

## F2 — Bun single binary + brew (additive, *alongside* Node)

The appeal is real: `brew install fleetdeck`, one static binary, and an end to the Node-version dance — the `node:sqlite` floor already bites (22.5–22.12 can't boot the daemon; see [[local-dev-018-testing]]). But the plugin channel must stay Node.

- **Keep Node + `node:sqlite` for the plugin-embedded daemon.** The SessionStart hook launches the daemon under the Node that Claude Code already runs — the doctrine-critical, fail-open, no-native-deps, nothing-to-install path. A per-platform Bun binary there **regresses** "it just works with the Node already here."
- **Add a Bun binary for the *standalone board server* only** — the `npm i -g fleetdeck` / Coder / LAN-board use, precisely where a one-command `brew` install and no Node floor are a clean win. `bun build --compile` produces the binary; `bun:sqlite` replaces `node:sqlite`.

### sqlite is *not* the risk

The deep dive is good news: DB access is **strongly centralized** — `db.mjs`/`statements.mjs` are the only `node:sqlite` importers, and the API surface maps **~1:1** to `bun:sqlite` (no `backup()`, no UDFs, raw portable `BEGIN IMMEDIATE` strings). The adapter seam is **one import site + a class alias** (`DatabaseSync ↔ Database`). CI runs a `node:sqlite × bun:sqlite` matrix so the two channels can't drift. See [architecture](./architecture.md#runtime--db-seam-f2).

### The Bun risks the vision didn't name

| Risk | Why it bites |
|------|--------------|
| **`mdns.mjs`** (1,012 lines of `node:dgram` multicast) | Bun's dgram/multicast is its **weakest node-compat corner** |
| **`ws` package under Bun** | Bun's native WebSocket server has different semantics than the `ws` package we use |
| **`child_process` + tmux control-mode** | long-lived control pipes under Bun are unproven for us |
| **Embedding `board-dist` assets** | packing the built board into a `--compile` binary is its own build step |

**Verdict:** Bun is an additive **distribution** win (brew, single binary), **not** a core runtime swap. Keep F2 **last and explicitly cuttable** — *"1.0 ships without brew if compat drags."* The release must **not** hostage on Bun's dgram support. It pairs naturally with F1: write once in TS, ship two ways.

---

## Doctrine check

Both foundations are doctrine-neutral by construction. **F1** changes only the build (esbuild already strips TS; no new runtime dep, no change to "one bundled file, `node:sqlite`, no `npm install`"). **F2** is additive: the fail-open, loopback, launched-under-Claude's-Node plugin path is untouched; Bun serves only the standalone board, which is already an opt-in `npm i -g` / Coder use. Nothing here introduces a wrapper, a phone-home, or a model call.

## Definition of done

- **F1:** F1a `contracts/` module shipped and consumed by daemon + board; `tsc --noEmit` a required, green CI lane; runtime boundary validation live on hook and spawn bodies; F1b in force (new code TS, no big-bang, tests green).
- **F2:** either the standalone board ships as a `brew`-installable single binary with the two-runtime CI matrix green — **or** it is cleanly cut from 1.0 with a stated reason (which Bun compat gap, and the revisit condition).
