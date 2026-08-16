# P2 — The harvest surface: diff view + notes + per-turn checkpoints

*Part of [Fleet Deck v1.0](./README.md). The review deck we completely lack today — and the highest-value, lowest-risk pillar (pure git + SQLite + UI, zero external risk). Where the vision and review differ, the review wins.*

## Problem — we have no review deck

Today the loop is **spawn → observe status → mail**. We can see *that* an agent is working, `needs-you`, or idle — but there is **no way to see *what* it changed**, annotate that change, or **undo a bad turn**. When you run three agents on the same issue and one wins, the "compare and keep the winner" flow is manual: read each worktree by hand, delete the losers. This pillar is the arc from **watch** to **harvest** — the deck where you inspect, correct, and revert the work the fleet produced.

Three pieces, one arc: **diff on the card → per-turn checkpoints → notes that become one batched revision mail.** All of it is git + SQLite + a new renderer — no model calls, no external dependency, nothing that can fail-open badly.

---

## The foundation the vision got wrong: base ref is **not** recorded

The vision built the diff view on "the spawn's recorded start-from ref (we already record it)." **That line is false and must be deleted.** Verified in the tree (review fact-check #2):

- **No base ref/SHA is recorded anywhere.** The `spawns` table has `origin_url` / `requested_branch` / `branch_mode` but **no base column** (`db.mjs:154-185`).
- **cwd-mode `git worktree add -b` passes no start ref** (`spawns.mjs:1086`) — the branch is cut from whatever HEAD happens to be, unrecorded.
- `worktrees.mjs:145-225` computes a base (origin/HEAD) at **inspection** time — a *fallback*, not a *record*. It answers "what's a reasonable base to diff against right now," not "what did this spawn actually start from."

**The fix — and it must land first, before anything else in v1.0 (~1 day):**

- Add **`base_ref`** and **`base_oid`** columns to the `spawns` table.
- **Stamp them at spawn time** in `materializeBranch` / the worktree-add path — record the actual ref and resolved SHA the worktree branches from.
- For **adopted / legacy** sessions with no recorded base, fall back to `merge-base HEAD origin/HEAD` (already computed in `worktrees.mjs`) and label it **"inferred base"** in the UI — honest about the difference.

Land this **first** so that from day one of v1.0 every new spawn **accrues review-ready data**. Everything else in P2 reads this column; if it lands late, the diff view has nothing trustworthy to diff against for weeks of history.

---

## Piece 1 — diff on the card

A daemon route computes the change set for a spawn against its recorded base:

```
git diff --no-color base_oid..HEAD      # tracked changes
git status --porcelain                  # untracked / staged state
```

Rendered in the **`FileViewer` shell** — but the vision oversold "rendered in the existing FileViewer." Verified (review #3): `FileViewer` today is a **plain-text tree viewer** — **no diff mode, no syntax highlighting, no image rendering, and a 5,000-line DOM cap** (`FileViewer.jsx:150-184`).

So the diff surface is a **new renderer**, reusing only the modal shell:

- a **unified-diff / hunk model** (parse `git diff` into files → hunks → lines);
- **line anchors** so notes (Piece 3) can attach to a specific old/new line;
- **cap + paginate** large diffs (respect the DOM-cap lesson — don't render a 40k-line diff into the tree).

Deterministic, no new heavy dependency: the daemon shells `git`, the renderer draws hunks.

---

## Piece 2 — per-turn checkpoints (the T3 Code gift), done safely

The **Stop hook fires at every turn boundary** — that's the T3 Code insight: a free per-turn signal we already receive. The vision's "on Stop, tag a cheap git checkpoint" is right in spirit but **breaks the daemon** without the review's mechanics:

- **The daemon deliberately runs zero git per turn today.** Identity is LRU-cached with one fresh exec *per session lifetime* (`events.mjs:258-271`), and the **Stop hook has a 5 s timeout** (`hooks/hooks.json:97-102`). A synchronous `git add` + commit on every Stop would blow the budget and add latency to the agent's turn.

**Mechanics that keep it safe:**

- **Async, off the hook-response path.** `hookStop` (`events.mjs:589-617`) is the single choke point — **answer the hook first, checkpoint after.** The checkpoint must never sit in the request the hook is waiting on.
- **Never pollute the working branch.** Use a temp index + `git add -A` (captures untracked) + `git commit-tree` onto **`refs/fleetdeck/<callsign>/turn-<n>`** — no commits on the working branch, no stash pollution, a clean parallel ref namespace.
- **A per-session turn counter** — doesn't exist today (`sessions.events` counts *all* hooks, not turns). Add one.
- **Stop recurs within one logical turn.** When queued mail blocks a Stop (Claude delivers mail by answering Stop with `decision:'block'`, `events.mjs:592-606`), the hook fires again for the *same* turn. **Checkpoint only on *passing* Stops** (or dedupe on the turn id) — otherwise **every mail delivery mints a phantom turn**.
- **GC / retention.** Checkpoint refs are cleaned up on despawn; `retention.mjs` is the home.
- **Size guard.** Skip the checkpoint if the repo exceeds N or `git add` exceeds T ms — **degrade honestly** (label the turn "checkpoint skipped: large repo"), never hang.
- **Revert is gated on idle** — never while the agent is mid-turn.

**What it exposes on the card:** **"diff since spawn," "what changed this turn," and "revert this turn."** Git-only and deterministic. This is what makes the compare-the-winner flow *dramatically* stronger than Orca's manual delete-the-losers — you can walk an agent's turns, see each turn's delta, and roll one back without touching the rest.

---

## Piece 3 — notes → **one** batched mail

Annotate diff lines directly on the card. On send, compose **one line-anchored mail** to the agent — **not a drip**.

This is Orca's own hard-won lesson: **dripping feedback makes the agent swing back and forth**, re-editing between each note. **One batch gives one revision pass and a higher hit rate.** So the interaction is: collect all notes → compose a single mail that quotes each anchored line + the note → send once.

It rides the **existing mail transport verbatim** — mail is already leased + acked (`/mail/ack`), so there is no new delivery machinery, only the composition step.

---

## Cross-provider note

Codex's hooks also carry a **`turn_id`** on turn-scoped events (see [P1](./p1-codex-provider.md)). That gives per-turn checkpoints a **parity path for Codex Tier A**: checkpoint on Codex's `Stop` + `turn_id` exactly as for Claude. The diff view is provider-agnostic already (it only needs `base_oid` + a worktree), so Codex cards get the harvest surface for free once the base ref is recorded.

---

## Doctrine check

- **No model calls** — git + SQLite only; the intelligence stays in the agent that reads the batched mail.
- **Deterministic** — diff, checkpoint, and revert are pure git operations, reproducible from the recorded base.
- **Fail-open** — the size guard degrades honestly, and a checkpoint failure **never blocks the Stop hook** (it runs after the response). A broken checkpoint costs you one turn's undo, not the agent's progress.

---

## Risks & open questions

- **Async checkpoint cost under load** — 15 sessions each checkpointing per turn is a real git budget; see [validation-and-gates](./validation-and-gates.md) §Performance bars. The size guard is the pressure valve.
- **Huge repos / huge diffs** — cap + paginate the renderer; skip-and-label the checkpoint.
- **Ref-namespace collisions** on callsign reuse — namespace by callsign *and* spawn id if callsigns can recur.
- **Revert semantics** across untracked files — `commit-tree` with `git add -A` captures them, but restoring must be explicit about what it touches (idle-gated, previewed).

---

## Validation & definition of done

**Determinism proof:** diff / checkpoint / revert are git-only and reproducible from `base_oid` — the same spawn state yields the same diff every time.

**Fail-open proof:** a checkpoint failure or a tripped size guard never blocks or delays the Stop hook; the card shows an honest "checkpoint skipped" rather than hanging.

**Acceptance:**

- `base_ref` / `base_oid` recorded on **every new spawn**; adopted/legacy sessions show an "inferred base";
- **diff-since-spawn renders** in the new hunk renderer with line anchors;
- **per-turn checkpoints accrue off the hook path** and add **zero** hook-response latency; only passing Stops mint a turn;
- **revert restores a prior turn** when the agent is idle, and is refused mid-turn;
- **notes compose exactly one mail**, line-anchored, over the existing transport.
