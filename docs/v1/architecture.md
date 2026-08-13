# Architecture — the provider seam and the platform backbone

*Part of [Fleet Deck v1.0](./README.md). This is the shared technical spine that F1, F2, P1, P4, and P7 all ride. Read it before any pillar doc: the seam decision here is the one that makes multi-provider — and multi-provider *drive* ([P7](./p7-drive-and-observe.md)) — tractable instead of a smear.*

## The one decision that matters: normalize at intake, not in derive

The vision said "`derive.mjs` becomes provider-aware." The review verified that this **mislocates the work**:

- the status transition switch lives in **`events.mjs:273-424`**, not `derive.mjs`;
- Claude-coupling smears across **≥6 modules and two side-channels** (`claude agents --json`, transcript JSONL);
- nine distinct Claude-coupling categories were catalogued in the tree.

If we parameterize `derive`, every one of those nine categories stays coupled and the state machine grows a `provider` branch in a dozen places. Instead:

> **Canonical-event normalization at hook intake + a per-provider strategy object.**

Hook payloads are the *only* churn-exposed boundary (both vendors ship experimental, changing hook schemas). Pinning a **canonical vocabulary** in typed contracts turns upstream churn into a *mapping bug at the edge*, not a *state-machine bug in the core*. And that canonical vocabulary **is** the contracts-first F1a work — so **F1a and the P1 refactor are one task**, sequenced together.

---

## Layer 1 — the contracts module (F1a)

A `contracts/` module (T3 Code's `packages/contracts` model) holding the typed shapes for every boundary, consumed by the daemon, the board, *and* the future Bun binary:

- the `/state` snapshot (today hand-mirrored in `board/src/useFleetState.js:25-41` with the contract living in *comments* on both sides of the wire — the motivation is literally that the contract is prose today);
- **canonical hook events** (below);
- `/api/spawn` body, mail/command/questions wire formats, the completions rows (P5), the usage payloads (P4);
- a **`schema_version`** field on every wire shape from day one, so hooks, board, and daemon can detect skew (the run-nonce work was exactly this class of bug).

Two hard requirements the review pinned, because **esbuild strips types without checking them** and **no `tsc` exists anywhere in the repo today**:

1. a required CI lane running `tsc --noEmit` (or `checkJs`);
2. **runtime validation of hostile boundary JSON** — hook bodies and spawn bodies are attacker-reachable; static types do not validate wire input. Validate at intake, reject malformed, fail-open.

See [foundations](./foundations.md) for the F1a timebox and the F1b standing rule.

---

## Layer 2 — canonical event vocabulary

A frozen, provider-agnostic event set in `contracts/`. This is roughly what `events.mjs:273-424` already switches on — renamed and pinned:

```
session-start
prompt
tool-start
tool-end   { tool, files?, command?, failed? }
needs-you  { kind, payload }
turn-end
session-end
file-changed
cwd-changed
```

Plus, on every event: `provider` and `schema_version`.

The `events` table gains a **`provider`** column (sessions already carry `source`, `db.mjs:49`). `applyEvent` consumes canonical events only and **stops knowing provider names**.

---

## Layer 3 — provider adapters at the HTTP boundary

Each vendor gets a thin intake adapter that maps *its* vocabulary onto the canonical stream:

- **`/hook/:event` (Claude)** — keeps today's shapes, maps → canonical.
- **`/codex-hook/:event` (Codex)** — new route; maps Codex's vocabulary → the *same* canonical stream:
  - `PostToolUse`(shell) → `tool-end{command}` (Bash-shaped telemetry is exactly what Codex exposes — the test-runner regex → `verifying` still works)
  - `PermissionRequest` → `needs-you`
  - `Stop` + `turn_id` → `turn-end`
  - `SessionStart`/`UserPromptSubmit` → `session-start`/`prompt`
  - **no** `apply_patch`/edit/MCP telemetry → **no** `file-changed`, **no** `tool-end{files}` (see the honest limits in [P1](./p1-codex-provider.md))

Because normalization happens here, the entire downstream state machine (`applyEvent`, derive, retention, the board) is written once and is provider-blind.

---

## Layer 4 — the provider strategy object

The event stream is only half the coupling. The other half — the piece the vision missed entirely — is the **non-event** Claude-coupling: argv, resume, pane identity, transcript paths, liveness. Model it as one strategy object per provider:

| Method | Claude (extract from existing code) | Codex (implement subset) |
|--------|-------------------------------------|--------------------------|
| `spawnArgv(opts)` | today's `claude …` argv | `codex …` argv |
| `resumeArgv(row)` | `claude --resume` | `codex resume` |
| `paneCommandName` | `'claude'` — fixes the hardcoded gates in `mail.mjs:358,402`, `spawns.mjs:1738` | `'codex'` |
| `transcript{path,lastAssistantText,model}` | fixes the `~/.claude/projects/…` hardcode (`helpers.mjs:25-27`) — **and is the P4 multi-account seam** (same abstraction) | rollout JSONL reader |
| `livenessPoll?` | `claude agents --json` (`agents-poll.mjs:38`) | pane-only |
| `needsYouAnswer(kind)→wire` | Claude's needs-you wire (display on the floor) | display on the floor |
| `nudgeGate` | trust-dialog regex (`spawns.mjs:319`) | Codex bring-up copy |
| `usageReader` | `~/.claude` reader (P4) | rollout-tail reader (P4) |

The table above is each provider's **observe-floor** strategy. Claude's is **extracted** from existing code (no behavior change); Codex's **implements the Tier A floor subset** and returns `"unsupported"` for what it can't honestly derive — **cards render exactly what the active strategy supports, honestly**. This is the single abstraction that makes both "first-class Codex" and "multi-account config-home pinning" tractable: `transcript{path}` becoming a per-session property is the same refactor P4 needs.

### The drive override (Layer 4 + P7)

The strategy object has a second dimension: a provider can be **driven** as well as observed ([P7](./p7-drive-and-observe.md)). Driving does not replace the strategy — it **overrides a handful of its fields** while the Layer-3 intake keeps firing unchanged, so the observed card is byte-identical and the *control* surface is added on top:

| Field | Floor (observe) | Drive override |
|-------|-----------------|----------------|
| `spawnArgv` / `resumeArgv` | launch / `--resume` the bare CLI | launch the driver (Claude: Agent SDK `query()`; Codex: `codex app-server` child) and resume via session id / `thread/rollback` |
| `paneCommandName` | `'claude'` / `'codex'` | the runner's pane command (runner-in-a-pane, [README rule 5](./README.md#the-doctrine-evolved)) |
| `needsYouAnswer` | display-only | **answerable** — Claude `canUseTool`; Codex `item/{commandExecution,fileChange}/requestApproval` |
| `transcript` | JSONL reader | SDK session id / app-server stream (partial-message tailing) |
| `livenessPoll` | CLI poll / pane-only | the driver runtime's own liveness |
| *(new)* `interrupt` / `steer` | n/a on the floor | Claude `Query.interrupt()` + streaming-input; Codex `turn/interrupt` + mid-turn `turn/start` |

Two driven providers exist: **`claudeSdk`** (drive-default now — [P1 — Claude Code](./p1-cc-provider.md)) and **`codexAppServer`** (staged after Claude, its hooks must stabilize first — [P1 — Codex](./p1-codex-provider.md)). When a driver is down or unavailable, the strategy falls back to its floor with no field overrides — the fail-open path that keeps a broken driver from darking the fleet. See [P7](./p7-drive-and-observe.md) for the full drive design and doctrine.

### The nine Claude-coupling categories (what the strategy unwinds)

Catalogued in the tree, so nothing is missed:

1. status derivation switch (`events.mjs:273-424`)
2. column writes across `events`/`derive`/`ingest`/`retention`/`spawns`
3. owned-pane mail requires `pane_current_command === 'claude'` (`mail.mjs:358,402`)
4. `/rc` requires `'claude'` (`spawns.mjs:1738`)
5. bring-up nudge parses Claude's trust-dialog copy (`spawns.mjs:319`)
6. revive is `claude --resume`
7. liveness rides `claude agents --json` (`agents-poll.mjs:38`)
8. `/clear` succession is Claude-semantic (`derive.mjs:503-715`)
9. transcript path hardcodes `~/.claude/projects/…` (`helpers.mjs:25-27`)

Categories 1–2 dissolve into intake normalization; 3–9 become strategy methods.

---

## What stays provider-free (the membrane holding)

The SQLite store, callsigns/tickets, worktrees, the mail queue + transport, questions, plans, completions (P5), and the board. If any of these grows a `provider` branch, the seam has leaked — treat that as a design bug.

---

## Runtime & DB seam (F2)

Good news from the deep dive: DB access is **strongly centralized**. `db.mjs`/`statements.mjs` are the only `node:sqlite` importers (113 statements live in the `statements.mjs` map, 35 in `db.mjs`, only **28 direct uses across 7 other modules** — mostly `BEGIN/COMMIT` strings + `questions.mjs`). The API surface maps ~1:1 to `bun:sqlite` (no `backup()`, no UDFs, raw `BEGIN IMMEDIATE` strings — portable).

- **The seam is one import site + a class alias:** `DatabaseSync` ↔ `Database` behind a single factory in `db.mjs`.
- **CI a 2×2 matrix:** `{node:sqlite, bun:sqlite}` × supported runtime versions.
- **The real Bun risks are not sqlite** (see [foundations](./foundations.md) §F2): `mdns.mjs` (1,012 lines of `node:dgram` multicast — Bun's weakest node-compat corner), `ws` under Bun, tmux control-mode long-lived pipes, and embedding `board-dist` assets in a compiled binary.

Hooks are runtime-agnostic — they just POST HTTP — so only the *daemon* runtime is ever in question. The plugin-embedded daemon **stays on Node + `node:sqlite`** (the doctrine-critical fail-open path launched under Claude Code's own Node); Bun is *additive* for the standalone board only.

---

## Migrations & versioning (the omission the review flagged)

Today there are ~30 ad-hoc `ALTER TABLE` migrations as column-introspection branches with **no `user_version` and no transaction around the migration block** (`db.mjs:261-386`). Workable at today's churn; **not** at v1.0's, which adds ≥4 tables/columns: base ref (P2), checkpoint refs (P2), completions (P5), event stream (P6), accounts (P4).

**Adopt for 1.0:**

- numbered, **transactional** migrations gated on **`PRAGMA user_version`**;
- a stated **compatibility rule** for each skew direction — old daemon + new board? new hooks + old daemon? (the SessionStart shim already prefers a committed bundle) — and a **downgrade answer**;
- **`schema_version`** carried in canonical events *and* in the daemon-served control skill (P5), so agents and hooks detect skew instead of silently misbehaving.

This is small, near-free plumbing, and 1.0 is its natural moment — every new pillar adds schema.

---

## How the pieces sequence

The architecture work is **not** a separate phase. It lands as:

- **F1a** = the contracts module + canonical vocabulary (phase 1, timeboxed);
- **P1 intake normalization + strategy extraction** = the same task, continued (phase 3) — extracts each provider's **observe floor** ([P1 — Claude Code](./p1-cc-provider.md), [P1 — Codex](./p1-codex-provider.md));
- **the drive override (`claudeSdk` / `codexAppServer`)** = layered on that same strategy object once the floors exist — the [P7](./p7-drive-and-observe.md) work; Claude drive-default first, Codex after its hooks stabilize;
- **DB seam** = one factory edit, deferred to **F2** (phase 7, cuttable);
- **migrations** = adopted incrementally as each pillar adds schema, starting with P2's base-ref column in phase 1.

See the [README sequencing](./README.md#sequencing-to-10-revised) and [validation-and-gates](./validation-and-gates.md).
