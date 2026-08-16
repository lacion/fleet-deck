# P5 — A programmable fleet: waitable completions + a privilege model

*Part of [Fleet Deck v1.0](./README.md). Turn the coordination nouns we already own into a substrate a coordinator *session* can drive. A small primitive (waitable completions) — and the chapter the vision skipped entirely: **privilege.** This pillar carries the review's single most actionable security finding (a verified P0).*

## What we already own

Fleet Deck already exposes `/api/spawn`, `/command`, `/mail`, `/state`, and the `fleet-doctrine` skill already teaches agents to `curl /mail` and `GET /state` (`SKILL.md:95-122`). The coordination *nouns* exist; what's missing is a blocking primitive and a real capability model. Two additions complete the substrate.

**The guardrails already exist** (verified, review #15): the spawn red asks-twice unsupervised gate is **server-side** — a single-use 60 s arm token that also gates revive & adopt (`spawns.mjs:112-141,1150-1158,1501-1508`) — and the four sanctioned keystrokes are real (`spawns.mjs:303-307`). We build on those, we don't reinvent them.

---

## T0.1 — waitable, typed completions (the one primitive we lack)

Mail is **fire-and-forget** on the sender side (confirmed, review #11): there is no recipient ack that a *task* is done. So a coordinator can dispatch work but cannot **block until it lands** — it has to poll `/state` and guess.

The primitive: a worker posts **`done` / `blocked` / `question`** against a **task id**; a coordinator **long-polls for the next unacked completion and blocks until it arrives.** Useful to a human-driven board too — *"wake me when this worker is done."*

**Build it as a sibling of `questions`** — there are **three in-house precedents to copy**, so this is assembly, not invention:

- transport is already **leased + acked** (`mail.mjs:470-495`, `/mail/ack`);
- `questions` are already **typed durable records** with `status` + `answer_json` (`db.mjs:142-153`);
- `/api/watch` is already a **25 s long-poll with a waiter registry** (`http.mjs:684-720`).

**Spec:**

- typed rows: `task_id, session_id, kind (done|blocked|question), payload, status, acked_at`;
- `GET /orchestration/check?wait&types=…` — long-poll copying the `/api/watch` waiter registry;
- **two failure semantics the vision skips:**
  - **dead-worker synthesis** — if a worker goes offline / is tombstoned with an **open** task, synthesize a `blocked` completion, or **the coordinator hangs forever**;
  - **idempotent ack by id** — a redelivered or double-acked completion is a no-op, not a crash.

---

## The privilege model — the gap, sharpened and verified (this is the P0)

The vision never mentions authorization for the control surface. The review verified it's **worse than "one flat token."**

On **default config** (`REQUIRE_TOKEN` off), plain-loopback callers get the historical exemption for **everything except three named power routes** — `/ws/term`, `POST /mail`, and `arm-unsupervised` (`http.mjs:336-348`). But:

- **`POST /api/spawn` is *not* one of the three** — it is **tokenless on loopback**;
- a spawn body may carry **`setup_cmd`**, which runs **`sh -c "$cmd"` *before* `claude` starts** (`spawns.mjs:20-25`).

**So on the default config, any same-UID process — or any permission-gated agent allowed a single `curl` to localhost — gets arbitrary shell execution, outside every permission mode, with no token and no human gate.** The code comment even names *"a fleet agent itself"* as the attacker the power gates exist for — **but spawn-with-`setup_cmd` isn't gated.** The asks-twice arm token protects the *`bypassPermissions` flag*, not code execution.

This is acceptable as a **known same-UID trust-zone residual today** ([[fleetdeck-security-standing]]). It is **not** acceptable once **P5 makes agents first-class API drivers** and **[P3](./p3-issue-pr-spawning.md) pipes third-party forge text into prompts** — that's the injection → unsupervised-spawn chain. P5 and P3's agent-drive/write surfaces **ship together with this model or they wait.**

**Minimum for 1.0 — one column + one middleware check, not a rewrite:**

- **Token-gate `POST /api/spawn`** — at minimum when it carries `setup_cmd` or a bypass mode — add it to the token-gated power routes.
- **Two token classes:**
  - **worker** — `mail` / `state` / `completions`;
  - **operator** — `spawn` / `kill` / `arm` — **and the [P7](./p7-drive-and-observe.md) drive controls** (approve / interrupt / steer / resume a driven session) — held by humans and by **explicitly-blessed** coordinator sessions only. A worker token can post completions and mail; it can never seize the wheel of a driven peer.
- **Cap agent-initiated spawns.** `spawnCapability` has **no cap today** (`spawns.mjs:232-236`) — add a **per-hour quota** so a runaway or hijacked coordinator can't fork-bomb the fleet.

---

## T0.2 — the daemon-served, versioned control skill

Extend `fleet-doctrine` from "mail + read state" to the **full deterministic surface** — roster, spawn, assign, mail, **wait** — as a **thin, versioned skill the daemon serves**, so documented flags **can't rot** against the running daemon.

**This is net-new plumbing** (review #14): today the skill is **static, shipped in the plugin cache, not served by the daemon**, and not even in the npm tarball (`package.json:10-15`). Serving it from the daemon and carrying a **`schema_version`** (see [architecture](./architecture.md)) lets agents **detect skew** — the run-nonce work was exactly this class of bug.

The skill documents the token classes above, so a coordinator session knows which capability it holds and what it may call.

---

## Doctrine check

- **No model calls** — completions are SQLite rows + a long-poll; the coordinator's intelligence lives in the *agent*, not the daemon.
- **Loopback preserved** — the control API stays local; token classes are an *authz* layer, not a network change.
- **The doctrine sharpening** — the human "asks-twice" gate was sufficient when only humans drove the board. Once agents are first-class drivers, it must become a real **capability model**. This pillar is where doctrine rule "privilege model, not just the gate" (see [README](./README.md#the-doctrine-evolved)) is cashed out. Cross-link the [P3](./p3-issue-pr-spawning.md) injection boundary — they are two halves of one threat.

---

## Risks & open questions

- **Coordinator hangs on dead workers** — mitigated by dead-worker synthesis; needs a tombstone signal from the liveness path.
- **Token-class migration** for existing loopback callers — default config today is tokenless; introducing classes must not break the human-at-the-board single-machine case (loopback human = operator by default, agents = worker unless blessed).
- **Spawn-cap tuning** — too low throttles legitimate fan-out (P3 issue batches); too high defeats the purpose.
- **Skill / flag skew** — mitigated by `schema_version` on the daemon-served skill.

---

## Validation & definition of done

**Authz / exposure proof:** on default config, an **unblessed loopback caller cannot reach spawn-with-`setup_cmd`** without an operator token; agent-initiated spawns hit the **per-hour cap**.

**Acceptance:**

- a worker posts `done` / `blocked` / `question` against a task id;
- a coordinator long-poll **blocks, then wakes** on the next unacked completion;
- a **dead worker synthesizes `blocked`** (coordinator never hangs); **ack is idempotent**;
- the daemon **serves a versioned control skill** with `schema_version`;
- **two token classes are enforced**, with the spawn power route gated and a spawn cap in place.
