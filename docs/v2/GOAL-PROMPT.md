# Goal-mode prompt for building Fleet Deck v2

*For pi-web-ui goal mode (goal-service 2.0). Written 2026-10-11.*

## How goal mode runs (facts that shape the prompt)

- The conversation where you set the goal is the **reviewer**. The server starts one resident **executor** conversation that does the work.
- Each round: the executor gets the goal text (+ the last review feedback), works, and writes a one-paragraph summary. The reviewer then checks the workspace with read-only tools and returns `{"verdict":"pass|fail"}`.
- The executor **cannot ask you questions and cannot start subagents**. The AGENTS.md worker/adversary protocol cannot run inside the loop.
- The loop stops ("blocked") after 2 rounds in a row with no change in `git diff HEAD` + `git status --porcelain`, 2 rounds with the same error, or 2 failed executor rounds. If the executor commits all its work at the end of a round, the next empty diff counts as "no progress".
- The reviewer's built-in rules judge only what the executor *claims* this round. Without an explicit rule, a partial round can pass and end the loop early.
- `maxRounds`: 0 = unlimited, otherwise at most 50.

## Before the first goal

1. Apply the P1 lessons of `t3code-lessons.md` to `PLAN.md` (architect + advisor review of L1 and L14).
2. Move `docs/v2/` into the `v2` worktree and commit it: `/home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2`.
3. Open a pi-web-ui conversation **with the `v2` worktree as the project directory**. The reviewer reads `git diff` from this directory.
4. Reviewer (this conversation) model: `gpt-6.1-sol`, effort high. Executor model: `claude-opus-5-5`, thinking high.
5. Run one goal per milestone. Between milestones, the owner and the architect do the gates below.

| Milestone | PLAN.md phases | maxRounds | Gate after it (outside the loop) |
| --- | --- | --- | --- |
| M1 | Phase 0 (spikes S1, S2, S4) | 15 | Owner accepts ADR-001/002/004; adversary DESIGN REVIEW of ADR-004 + §8 + §9 |
| M2 | Phases 1–5 (foundations → Codex) | 50 | Adversary CODE REVIEW of the branch; owner tries the MVP demo |
| M3 | Phases 6–7 (fleet features → hardening, cutover on the branch) | 50 | Adversary PRE-PR REVIEW, owner sign-off, merge to `main`, release `2.0.0` |

If a milestone hits 50 rounds, set the same goal again; `PROGRESS.md` carries the state.

## The goal text

Replace `<M>` with `M1`, `M2`, or `M3` before you paste it.

```text
Build Fleet Deck v2 milestone <M> in this repository (branch v2), exactly as specified in docs/v2/PLAN.md.
Milestones: M1 = PLAN Phase 0. M2 = PLAN Phases 1-5. M3 = PLAN Phases 6-7, without the merge to main and without the release.

SOURCES OF TRUTH
- docs/v2/PLAN.md: owner decisions D1-D14 and architect decisions A1-A13 are binding. Do not change them.
- docs/v2/t3code-lessons.md: design input; the rows already applied to PLAN.md are binding through PLAN.md.
- docs/v2/adr/*.md with status "Accepted" are binding. In M1 you write them with status "Proposed".
- If the plan is wrong, incomplete, or conflicts with reality, do not stop and do not change a decision silently. Add an entry to docs/v2/DECISIONS-LOG.md (context, options, choice, reason, "owner review: pending"), choose the option most consistent with the doctrine (PLAN §3), and continue.

HOW YOU WORK (you are the only implementer; the loop forbids subagents and questions, so the AGENTS.md delegation protocol is suspended for this run)
1. Start every round: read docs/v2/PROGRESS.md (your memory) and the review feedback. If the working tree has changes from the previous round, commit them first with a Conventional Commit message.
2. Do the next unchecked item of <M> in PROGRESS.md. Create PROGRESS.md in round 1: one checklist item per deliverable of <M>, in PLAN order, each small enough for one round.
3. Do not commit the work of the current round. Leave it in the working tree for the reviewer. Update PROGRESS.md every round (item status, commands run, results), so each round has a visible diff.
4. Before the summary, run the checks that exist so far and fix all failures:
   cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo nextest run --workspace; cargo deny check;
   in web/: bun run typecheck, bun run lint, bun run test. Also the TS-types drift check (PLAN §11).
5. End the summary with exactly one of:
   STATUS: IN PROGRESS - done this round: <items>; checks: <command -> result>; next: <item>
   STATUS: MILESTONE COMPLETE - evidence: <each acceptance item -> command -> result>

RULES
- Rust stable 1.96, edition 2024; crates and stack from PLAN §12. Ask for no new choice: if a crate in §12 does not work, log it in DECISIONS-LOG.md and pick the closest maintained alternative.
- Modules stay under 800 lines. No `unwrap()` outside tests. Secrets use Secret<T>. Unknown permission requests fail closed.
- Real `claude` and `codex` CLIs are installed. Use them only for spikes, fixture recording, and live smoke tests: tiny prompts, a temporary directory, never bypass/full-access mode outside a temporary directory. Normal tests use fakes and recorded fixtures only.
- The TypeScript Agent SDK may be installed only in spikes/ to record reference frames (D8). It never ships.
- Docker is not installed. Multi-station and end-to-end tests run several fleetdeck processes on loopback with separate data directories and ports. Simulate NAT with a station that has no listener and only dials. Tailscale is not installed: test the tailnet address rules with unit tests; a real tailnet check is an owner step.
- Do not modify v0 code (src/, board/, hooks/, scripts/, plugin files) except in the M3 cutover step, which is the last item of M3.
- Never push, never open a pull request, never merge into main, never tag a release.

ACCEPTANCE FOR <M>
- M1: spikes/ contains runnable spikes S1 (Claude stream-json from Rust, including replay of frames recorded from the TypeScript SDK), S2 (codex app-server from Rust), S4 (link: both directions, mTLS pinned keys, invitation pairing, control + data channels, reconnect, station-only-dials case). docs/v2/adr/ADR-001, ADR-002, ADR-004 exist with status Proposed and cover every "Must prove" row of PLAN §14 Phase 0. The PLAN §6.2 capability matrix contains verified values with the evidence (fixture or test name) for each cell. Each spike has a documented command that the reviewer can run without network access (replay mode).
- M2: every exit check of PLAN Phases 1, 2, 3, 4 and 5 passes as an automated test, including: crash-before-ack fault tests; a deck with 2 stations (one dials in, one is dialed) where link drops and deck restarts lose no events and duplicate no commands; login required also on loopback; public listen and dial addresses are rejected; the MVP demo flow (spawn on two stations, answer an approval, steer, interrupt, survive a deck restart and a network drop) as a Playwright test at a phone viewport; the same board flows for Claude and Codex; the capability matrix covered by tests.
- M3: every Phase 6 item and every Phase 7 item of PLAN §14 is done, except the adversary review, the owner sign-off, the merge, and the release. Every row of the PLAN §16 parity table is done or marked deferred in DECISIONS-LOG.md with a reason. The human shell (§6.8) works with its scrollback cap and reattach. Performance bars of PLAN §13 are measured against fake agents and recorded in docs/v2/perf.md. docs/v2/internals.md and the new README exist. The cutover step removed the v0 code on this branch and all checks still pass.
- All milestones: all checks in step 4 pass on a clean checkout of the committed work plus the current round; DECISIONS-LOG.md lists every deviation.

REVIEWER RULE
The goal is satisfied only when the executor reports STATUS: MILESTONE COMPLETE and you have rerun the acceptance commands yourself with passing results. A round that reports STATUS: IN PROGRESS is never a pass, even if its items are correct: return fail with feedback "continue: <next item>" plus any concrete defects you found. Fail any claimed item that has no command output or file evidence.
```
