# P4 — Usage, rate-limits, and multi-account: the operations deck

*Part of [Fleet Deck v1.0](./README.md). The number that governs a 100x day, plus spreading load across accounts. Split into two differently-sized halves. Where the vision and review differ, the review wins.*

## Problem — flying blind on the meter that governs a 100x day

At fifteen agents, the binding constraint is not attention — it's **quota**. Today Fleet Deck shows none of it:

- **No usage or rate-limit visibility.** An operator can't see how much of a 5-hour window a session has burned, which session is about to trip a limit, or what the fleet has spent in aggregate. The first sign of exhaustion is an agent stalling.
- **No way to spread load across accounts.** If one OAuth home is minutes from reset, there is no supported move but to wait — even when a second account sits idle.

The vision named both. The review's correction is that these are **two halves of very different sizes**, and conflating them hides the fact that one ships cleanly in 1.0 while the other is a cross-cutting refactor. This doc splits them.

**One honest caveat is baked into everything below:** these numbers update **when the agent writes its local state**, not in real time. Fleet Deck reads files the harness already produces; it never calls a metering API. A meter that lags a few seconds and says so beats a "live" number that required phoning home.

---

## Half 1 — the usage meter (ships in 1.0, needs nothing from P1)

A read-only meter over local state. **No API, no extra auth.** Sorted tightest-window-first, so the session about to trip is always on top.

### Claude usage meter — greenfield but well-understood

The mechanism is Orca's, and it's simple: the harness writes usage/session state under `~/.claude`; read it, don't ask a server.

- **Windows:** surface the **5-hour / daily / weekly** reset windows, each with time-to-reset.
- **Warning chip at 80%.** Per window, a chip flips at 80% consumed — the single glance an operator needs.
- **Burn:** per-session and **fleet-total** burn.
- **Cost:** derived from a **local price table** ("inferred pricing") — a shipped table of per-model rates, applied to local token counts. Labeled inferred, never billed-truth.
- **Sort:** **tightest-window-first** across the fleet.

**Honest correction to the vision.** The vision implied Fleet Deck already reads usage state. It does not: today the daemon reads exactly **one** thing under `~/.claude` — session transcripts (`transcript.mjs`, `helpers.mjs:25-27`). Usage reading is **greenfield**. The corollary that governs the whole pillar: **absence = "unknown"**, never a guessed zero. If the file isn't there or hasn't been written, the meter says so.

Ship this **early** — it is fully independent of P1 (Codex).

### Codex usage — burn yes, limits often null

Codex writes rollout JSONL that carries `token_count` events, so **burn is readable**. Limits are the problem, per the review's external verification:

- recent Codex builds write **`rate_limits: null`** ([openai/codex#14880](https://github.com/openai/codex/issues/14880)) — local files often **lack reset windows entirely**, or lag hours;
- rollout files **balloon** (20 MB+ observed) → **tail, never slurp** (already the doctrine).

So the meter needs a first-class **"unknown — never guess"** state **per window**: burn can render while a window's limit shows `unknown` beside it, honestly. Consider **`auth.json`** as a secondary source for windows — community tools ([codex-auth](https://github.com/loongphy/codex-auth), [codex-check](https://github.com/Leask/codex-check)) fall back to it when rollout limits are null.

### CLIProxyAPI — consume the queue, don't just sit beside it

Fleet Deck already routes per-session through a gateway, with **CLIProxyAPI (CPA)** as the reference. The vision framed the relationship as "CPA dropped usage stats, Fleet Deck adds the missing half." The review found a stronger move:

- CPA **v6.10.0 removed** built-in usage statistics — but **replaced** them with a consumable **usage queue** (RESP, and `/v0/management/usage-queue` over HTTP in **v6.10.8+**), *explicitly designed for companion tools*.
- So for sessions already gateway-routed, Fleet Deck should **also consume the queue** as an optional, richer usage source — becoming the companion dashboard CPA's own README says to pair it with.

| Source | Reads | Gives | When |
|--------|-------|-------|------|
| `~/.claude` local state | files the harness writes | Claude burn + windows | always (native homes) |
| Codex rollout JSONL (tailed) | `token_count` events | Codex burn; limits often `unknown` | always (native homes) |
| Codex `auth.json` | secondary | possible window backfill | when rollout limits null |
| **CPA usage queue** | `/v0/management/usage-queue` | richer per-key usage | **optional**, gateway-routed sessions only |

Layers are **complementary**: native homes for the always-on floor; CPA's queue as an optional richer feed where routing is already in place.

---

## Half 2 — multi-account (bigger than "same env discipline" — a STRETCH)

The vision said pinning is "via `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, the same per-session-env discipline as gateway routing." **Mechanically yes at launch** — the env map already flows to the tmux worker (`spawns.mjs:556-578`). But the review found it's bigger than an env var:

- **`CLAUDE_CONFIG_DIR` appears NOWHERE in the daemon today**, and the transcript probe **hardcodes `~/.claude/projects/…`** (`helpers.mjs:25-27`).
- Pin a session to a different home and three things **silently break**: revive-eligibility (`spawns.mjs:1249`), adopt (`helpers.mjs:77-85`), and freeform-question detection (`events.mjs:622-624`) — all of which assume `~/.claude`.

So account pinning **requires**, not "an env var," but:

1. a per-session **`config_home` column** consulted by **every** transcript-path consumer. This is *exactly* the `transcript{path}` method on the provider strategy object from [architecture](./architecture.md) — **the same refactor P1 and P4 both need.** Do it once, both pillars benefit.
2. **per-account usage attribution** — burn and windows bucketed by home, feeding Half 1's meter.
3. **hooks reporting `CLAUDE_CONFIG_DIR` back.** The hook runs *inside the session env*, so it can **self-report** the home it's using; the daemon **cannot infer it** from outside. The hook payload gains the config home; the daemon stamps it on the session.

The operator move this unlocks — the reason it's worth the refactor:

> *"Account A is 15 minutes from reset — launch this one on account B."*

Show **per-account limit bars** so that decision is one glance.

**Recommendation:** the **usage meter ships in 1.0**; account **pinning is a stretch**. If it slips, cut it cleanly — and name the **config-home / transcript-path refactor** as its explicit prerequisite so the cut is legible, not a surprise. (P1's strategy extraction pays down most of that refactor anyway, so a slipped-to-post-1.0 pinning inherits a shorter path.)

### Relationship to the sequencing

Per the [README sequencing](./README.md#sequencing-to-10-revised): **Claude usage meter lands in phase 3** (alongside P1, but independent of it), **Codex usage in phase 4**, and **account pinning rides P1's strategy extraction** — shipping only if the config-home consumers are all converted in time.

---

## Doctrine check

- **No model calls, no metering API, no extra auth.** The meter reads local files the agents already write; the CPA queue is an *optional* read of a gateway the user already runs.
- **Loopback preserved** — everything is served by the local board.
- **Multi-account credential handling reuses the gateway discipline EXACTLY** ([[fleetdeck-security-standing]]): the config home is a per-session property; **no account credential ever lands on the board, in `/state`, or on a process command line** (the `env -u` scrub + `token_set` masking pattern applies unchanged).

---

## Risks & open questions

- **Codex `rate_limits: null`** — the unknown-state design must be first-class, per window, from day one; don't paper over with a zero.
- **Ballooning rollout files** — tail, never slurp; cap the tail window.
- **Pinning's cross-cutting refactor** — every transcript-path consumer must migrate to `config_home`, or a pinned session degrades silently. This is why pinning is a stretch, not a "small env change."
- **Price-table drift** — inferred cost is only as good as the shipped table; label it inferred and version it with the daemon.
- **CPA queue coupling** — an optional source must degrade to native-home reads if CPA is absent or on an older version; never hard-depend on the queue.

---

## Validation & definition of done

- **Exposure proof:** no account credential appears in `/state`, argv, or logs — verified the same way the gateway token is (`env -u` scrub, `token_set` masking).
- **Honesty proof:** every window that cannot be known renders **"unknown"** — never a fabricated number, never a guessed zero. Codex sessions with `rate_limits: null` show burn with limits labeled `unknown`.
- **Fail-open proof:** if usage files are absent/unreadable, the meter degrades to "unknown" and the rest of the board is unaffected.
- **Acceptance:**
  - Claude usage meter **live** with the 80% chip, tightest-window-first sort, and per-session + **fleet-total** burn;
  - Codex **burn shown** with unknown limits clearly labeled;
  - *(stretch)* a spawn **pinned to account B** shows correct **per-account** limit bars **and does not break** revive, adopt, or freeform-question detection.
