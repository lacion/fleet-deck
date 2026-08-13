# P6 — Unified views: terminal grid, a Slack-style stream, and optional chat

*Part of [Fleet Deck v1.0](./README.md). The view that scales to "watch fifteen agents like a Slack workspace" — sized honestly. Where the vision and review differ, the review wins.*

P6 is the pillar most exposed to optimistic sizing, because two of its three pieces *look* like they already exist and merely need "promoting." They don't. The terminal grid **is** shipped; the stream is a **new event subsystem**; and chat has **no in-progress-turn source** in the tree today. This doc keeps the instinct and corrects the sizes.

---

## Problem — "see every terminal at once" is half-shipped

The desire is real and partly met. Fleet Deck already has the **Terminal Grid** (`▦ Terminals`): every live agent streaming at once, with one focused tile accepting input. That is the tiled view, and it works today.

What's missing is the *merged, human-readable* view — the thing that lets an operator watch a fleet the way they watch a Slack workspace, skimming a single feed instead of scanning a wall of terminals.

Worth stating plainly: **this is a Fleet Deck synthesis, not a port.** Orca's multi-agent view is split/tiled panes — it has no merged, channelized stream. So we are not copying a competitor's feature; we are building the view the tiled grid implies but never delivers.

---

## Feature 1 — the Slack-style activity stream (a **new event subsystem**, not a "promotion")

The vision framed the stream as "the board's `Feed` + mail, promoted into a first-class unified stream." That undersells the work by an order of magnitude. Verified against the tree, the Feed is not a substrate you can promote:

- The ticker is a **bare `(id, at, msg)` string table** (`db.mjs:123-127`) — no type, no session/repo association, no structured payload.
- Messages are **classified client-side by emoji prefix** (`util.js:515-522`) — the "type" of a feed line is a leading glyph, parsed in the browser.
- It keeps **500 rows, serves 40** — sized as a recent-activity ticker, not a durable channelized log.
- **Tool actions never land in it** — `PostToolUse` doesn't tick (`events.mjs:285-307`), so the richest signal a Slack-style stream would show isn't even captured today.

So the stream is a genuine subsystem. What it needs:

| Piece | What it is | Why the ticker can't do it |
|-------|------------|----------------------------|
| **Structured event table** | `(at, session_id, repo_id, type, payload)` written alongside the ticker | Ticker is an untyped string blob with no session/repo keys |
| **Channels as derived views** | per-session channel and per-repo channel, both computed from the table | No association columns exist to group by |
| **Retention policy** | explicit GC by age/count per channel | 500-row global cap ≠ per-channel history |
| **Read cursors** | per-viewer "last seen" so unread counts and catch-up work | Ticker has no notion of a reader |
| **Selective tool-action events** | a curated subset of tool actions, not the firehose | Full `PostToolUse` volume would swamp the table **and** the WS |

**The WS constraint is the sharp one.** The board broadcasts a **coalesced full snapshot every 60 ms** (`http.mjs:1370-1382`). Piping a full tool-action firehose through that model would multiply both the event-table write volume and the broadcast payload. This one surface therefore needs a **delta channel or a per-channel fetch** — it must *not* ride the broadcast-everything path. Treat "the stream reuses the existing WS snapshot" as a design bug.

**What the stream renders:** turn boundaries, tool actions, needs-you prompts, and mail — each as a message, grouped into a channel per session (and per repo). And crucially, **you can post into a channel** — which is just mail to that session, so the outbound path already exists (`mail`). Read + write, in one feed. This is the view that scales to fifteen agents: skim the merged stream, drop into a channel to reply, jump to the terminal tile only when you need the raw surface.

---

## Feature 2 — an optional chat interface (secondary; the terminal stays primary)

A per-card composer that renders the agent's current turn as chat and sends prompts as mail-to-pane. Two facts from the tree set the honest scope:

- **The outbound half already exists.** The Drawer has a per-card composer (this-tab-only, `Drawer.jsx:423-451`, `App.jsx:58`). Sending a prompt as mail is solved.
- **The inbound half is oversold.** The vision says chat is "projected from the hook/rollout events we already receive." Not quite: the transcript reader extracts **final assistant text only** (`transcript.mjs:69-104`). Rendering an **in-progress** turn — the token-by-token "the agent is typing" experience that makes chat feel like chat — needs a **live tailer subsystem that does not exist** in the tree.

So chat gets two honest options for 1.0 — **pick one, and keep it explicitly secondary either way:**

- **(a) Downscope to a turn-level thread.** Render final assistant text *per turn* plus the outbound mail, as a threaded conversation. Fully feasible from current reads (transcript final-text + mail history). No live tailer. You lose "watch it type"; you keep a readable per-card conversation.
- **(b) Demote chat to post-1.0.** Ship the stream (Feature 1) as the human-readable surface for 1.0 and defer chat until a live tailer is worth building.

Either way, the terminal remains the **authoritative** surface and the chat view **never becomes load-bearing**. This is the one place we borrow T3 Code's "better than a terminal" instinct — deliberately as an *addition beside* the terminal, not a replacement for it. The terminal that stays authoritative for a *driven* session is the **runner-in-a-pane** ([P7](./p7-drive-and-observe.md)), not this chat view: the moment chat displaces that rendered pane as the primary surface, we've drifted from plugin toward app.

**Recommendation:** ship (a) if the turn-level thread lands cheaply on top of the stream's event model; otherwise (b). Do not build the live tailer for 1.0.

---

## Cross-cutting tidy — consolidate the permission ladder

The vision described "replacing the single scary 'unsupervised' checkbox with a legible ladder." The tree shows the ladder is **not missing — it's duplicated:**

- the spawn form **already has** a four-mode dropdown — `default / acceptEdits / plan / bypassPermissions ⚠` (`SpawnForm.jsx:945-948`);
- **and** a separately-armed unsupervised checkbox, gated by the server-side single-use arm token.

So the real work is **consolidation**, not creation: fold two overlapping controls into one legible ladder —

**Supervised → Auto-accept-edits → Auto → Full-access**

— mapping each rung onto Claude's `--permission-mode` *and* **Codex's approval × sandbox grid** (see [P1](./p1-codex-provider.md)). The consolidation must preserve the existing server-side arm gate for the top rung (Full-access) — the ladder is a legibility change on the *surface*, not a loosening of the *gate*. This ties directly to the P5 privilege model: the ladder is what a human sees; the token classes and spawn caps are what actually enforce ([P5](./p5-programmable-fleet.md)).

---

## Doctrine check

- **Plugin, not app — preserved.** The stream and chat are board pages. The terminal stays primary and authoritative; chat is explicitly capped as secondary so it can't become the product.
- **Loopback / no phone-home — preserved.** Both surfaces are served by the local board over the existing WS/HTTP; no hosted relay.
- **No model calls in the core — preserved.** The stream is structured events (arithmetic + SQL + a derived view). Chat *projects* reads we already have (transcript final-text + mail); it does not call a model to render.
- **The stream stays an observe/comms surface, even though the fleet now drives.** Fleet Deck *does* mediate at 1.0 — it drives sessions through their native protocol ([P7](./p7-drive-and-observe.md)) — but that happens through the runner, never by typing in a channel: posting into a stream channel is still mail-to-pane, not a drive action. The observe thesis survives because the driven session's hooks keep firing, so every stream event is still pure derivation.

---

## Risks & open questions

- **WS broadcast pressure at 15 sessions.** The coalesced full-snapshot model (`http.mjs:1370-1382`) is the bottleneck; the stream must use a delta/per-channel path or it multiplies broadcast payloads. Carry a perf bar for this (see [validation-and-gates](./validation-and-gates.md)).
- **Event-table write volume + retention.** Selective tool-action events keep it bounded; the retention/GC policy needs a home (alongside the existing retention module) and a size budget.
- **Chat's in-progress-turn source.** The downscope-vs-demote decision is the open question; do not build a live tailer for 1.0.
- **Channel identity.** Per-session vs per-repo channels are both derived from `session_id`/`repo_id` on the event; confirm the derivation covers adopted/legacy sessions.

---

## Validation & definition of done

**Determinism proof.** The stream is derived entirely from structured events (no model calls); the same event log always produces the same channels. Chat, if shipped, renders only existing reads.

**Fail-open proof.** If the event subsystem is unavailable, the terminal grid and the ticker keep working — the stream is additive, never on the critical path.

**Acceptance criteria:**

1. A **per-session** and a **per-repo** channel each render turn boundaries, tool actions, needs-you prompts, and mail as messages, with **read cursors** driving unread state.
2. **Posting into a channel delivers mail** to that session (reusing the existing mail path).
3. Tool actions in the stream are a **selective subset**, delivered via a **delta/per-channel path** — not the 60 ms full-snapshot broadcast.
4. The permission ladder is **one consolidated control** (Supervised / Auto-accept-edits / Auto / Full-access) mapped across **both** providers, with the top rung still behind the server-side arm gate.
5. Chat, **if** shipped for 1.0, is a **turn-level thread** (final assistant text per turn + outbound mail) and is explicitly labeled the secondary surface — or is deferred to post-1.0.

**Test strategy.** Event-model unit tests (event → channel derivation, read-cursor math); a WS load test at 15 sessions to hold the perf bar; a permission-ladder mapping table test asserting each rung resolves to the correct Claude `--permission-mode` and Codex approval×sandbox pair.
