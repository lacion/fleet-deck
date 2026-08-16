# Foundations — the platform bets everything else rides on

*Part of [Fleet Deck v1.0](./README.md). See [architecture](./architecture.md) for the contracts module and the DB seam these two foundations feed. Where the vision and review differ, the review wins.*

Two questions to settle before the pillars land, because the pillars are heavier if the ground is soft. **F1** decides whether the growing multi-provider surface is held together by typed contracts or by comments and hope. **F2** decides the runtime the product ships on — resolved by going **Bun-primary, single-runtime**, which ends the Node-version dance outright. Neither is a rewrite; both are seams. Get them wrong and every pillar pays interest.

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

## F2 — Bun-primary runtime (single-runtime; **supersedes** the additive-alongside-Node stance)

> **Superseded (foundations-hardening, 2026-08).** The review below scoped F2 as *additive and cuttable* — a Bun `--compile` binary for the standalone board, **beside** a Node plugin path. That is no longer the plan of record. Fleet Deck went **Bun-primary and single-runtime**: the daemon, the standalone/dev path, **and the Claude Code plugin fail-open hook floor all run on Bun** — `hooks.json` execs `bun "…/scripts/fleet-*.mjs"`, commit `beb18cea` **deleted the Node version floor**, and `package.json` `engines` is now `bun >=1.3.14`. `node:sqlite` is **retired**: the store opens through `bun:sqlite` behind the `sqlite.ts` seam, and CI runs **one authoritative Bun test lane**, not a node×bun matrix. The reasoning below still holds; read its Node-vs-Bun specifics as corrected by this note. See [foundations-hardening](./foundations-hardening.md).

The appeal was always real: `brew install fleetdeck`, one runtime, and an end to the Node-version dance — the `node:sqlite` floor bit hard (22.5–22.12 couldn't boot the daemon; see [[local-dev-018-testing]]). The resolution went further than the review's "keep the plugin on Node": Bun now runs **everything**, plugin path included.

- **The plugin-embedded daemon and its hook floor run on Bun.** The SessionStart hook (`fleet-sessionstart`) and the per-event hooks (`fleet-hook`, `fleet-watch`) are invoked as `bun …` and bring the daemon up under Bun — still the doctrine-critical, fail-open, no-native-deps, nothing-to-install path, now with the Node-version floor **gone** rather than merely tolerated.
- **The standalone / `npm i -g` / Coder / LAN-board path is the same Bun runtime** — no second channel to keep in step. `bun:sqlite` replaced `node:sqlite`; an optional `bun build --compile` single binary + `brew` remains a *distribution* option on top of the one runtime, not a separate compatibility lane.

### sqlite is *not* the risk

The deep dive was good news and the seam landed clean: DB access is **strongly centralized** — one module (`sqlite.ts`, `openDatabase()`) is the only place either SQLite builtin is named (`statements.ts`/`db.ts` build on it), and the two drivers map **~1:1** (no `backup()`, no UDFs, raw portable `BEGIN IMMEDIATE` strings). The seam is **one import site + a class alias** (`DatabaseSync ↔ Database`) that now resolves to `bun:sqlite`; the historical `node:sqlite × bun:sqlite` matrix collapsed to a **single Bun lane** once the runtime unified. See [architecture](./architecture.md#runtime--db-seam-f2).

### The Bun risks the vision didn't name

| Risk | Why it bites |
|------|--------------|
| **`mdns.mjs`** (1,012 lines of `node:dgram` multicast) | Bun's dgram/multicast is its **weakest node-compat corner** |
| **`ws` package under Bun** | Bun's native WebSocket server has different semantics than the `ws` package we use |
| **`child_process` + tmux control-mode** | long-lived control pipes under Bun are unproven for us |
| **Embedding `board-dist` assets** | packing the built board into a `--compile` binary is its own build step |

**Verdict (updated):** the review called this an additive distribution win, *not* a core runtime swap, and wanted F2 kept last and cuttable. Events overtook that — Bun **became** the core runtime (foundations-hardening), single-runtime, floor included. What the review got right survives: the real risks were never sqlite but `mdns`/dgram, the WebSocket layer, tmux control pipes, and embedding `board-dist` — so those were the go/no-go gates the Bun spike had to clear before the swap committed. The optional `brew` single binary rides on top and stays cuttable; the *runtime* no longer is.

---

## Doctrine check

Both foundations are doctrine-neutral by construction. **F1** changes only the build (esbuild/Bun already strip TS; no new runtime dep — `package.json` `dependencies` is still `{}` — no change to "one bundled file, no `npm install`"). **F2** swapped the runtime, not the doctrine: the fail-open, loopback, launched-by-the-plugin path is intact — it now runs on Bun with `bun:sqlite` instead of Node with `node:sqlite`, still no-native-deps and nothing-to-install. Nothing here introduces a wrapper, a phone-home, or a model call.

## Definition of done

- **F1:** F1a `contracts/` module shipped and consumed by daemon + board; `tsc --noEmit` a required, green CI lane; runtime boundary validation live on hook and spawn bodies; F1b in force (new code TS, no big-bang, tests green).
- **F2:** the runtime is **Bun-primary and single-runtime** — daemon, standalone path, and the plugin hook floor all on Bun with `bun:sqlite`, the Node floor deleted, and the single Bun test lane green. (An optional `brew` single binary rides on top and stays independently cuttable.)
