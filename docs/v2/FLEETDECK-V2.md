# Fleet Deck v2: what it is, what we learned, what comes next

Status: owner draft, 2026-10-13. This document reads every v1 planning document of branch `fd/typescript-migration` and sets them against v2 as it is today, at commit `c7820bf` on branch `v2`. It keeps what still applies, names what v2 changed on purpose, and ranks what to build next.

Sources:
- `docs/fleetdeck-v1.0.md`
- `docs/fleetdeck-future.md`
- `docs/orca-lessons.md`
- `docs/t3code-lessons.md`
- `docs/ux-feedback-plan.md`
- `docs/v1/*`: README, architecture, design-spec, foundations, foundations-hardening, validation-and-gates, p1-cc-provider, p1-codex-provider, p2-harvest-surface, p3-issue-pr-spawning, p4-usage-accounts, p5-programmable-fleet, p6-unified-views, p7-drive-and-observe, ui-spec, ts-migration, phase3, phase4 and phase6.

Each claim about v2 below has a file or a test behind it. The per-item notes are in `docs/v2/research/v1-lessons/`; the important evidence is repeated here.

How this document relates to the other v2 documents:
- `PLAN.md` is the design of record.
- `DECISIONS-LOG.md` records each decision.
- `PARITY.md` maps each v1 feature to a test.
- This document is the product view. When it and `PLAN.md` disagree, `PLAN.md` wins until a DL entry changes it.

---

## 1. What v2 is

Fleet Deck v2 is a command deck for a fleet of coding agents (Claude Code and Codex) that run on one or more machines.

- **Stations** run on every machine that owns a workspace. A station starts each agent through the agent's own protocol: Claude `stream-json` and the Codex `app-server`. It stores every event durably and owns all execution: provisioning, git checkpoints, shells, worktrees and the setup command.
- **The deck** is one always-on hub. Stations pair with it and dial into it over an authenticated link. It keeps the read model, the mail outbox, tasks, plans, coordinator grants and the audit log, and it serves the board.
- **The board** is a React web app for desktop, tablet and phone. It drives sessions through chat, answers approvals, reviews per-turn diffs, reverts turns, opens terminals and hands a session over to the provider TUI.

What makes v2 different from v1:

| v1 (v0.22 – v1.0 plan) | v2 |
| --- | --- |
| Observes `claude` in tmux through hooks; a "drive tier" was a plan | Drives both providers natively; no tmux, no hooks, no screen scraping (PLAN D3) |
| One machine, a local daemon | A deck plus N stations; a station owns its machine (PLAN A1) |
| Node or Bun `.mjs` daemon | Rust: proto, link, station, deck, providers, CLI |
| Hand-written wire types | Rust types generate the board's TypeScript; a drift test guards them (`make ts-types`, `ts_drift`) |
| Hook events normalized ad hoc | One canonical event vocabulary at intake; a provider-blind core; replay fixtures per provider |
| Checkpoints were a plan | Per-turn hidden-ref checkpoints, a revert fence, reconciliation (`workspace/checkpoint.rs`, `revert_run.rs`) |
| Mail by blocking the Stop hook | Mail is a framed follow-up or a steer from a durable outbox |
| One "unsupervised" checkbox | A mode ladder from the provider's native modes (PLAN L8) |

---

## 2. The doctrine: the filter for every new feature

v1 had five doctrine rules, and v1.0 widened them. v2 keeps their intent but changes two of them: "observe, never drive" and "plugin, not app". A new feature must pass every rule below, or a DL entry must name the exception.

1. **Drive through the protocol.** The station speaks each provider's own protocol. It never scrapes a terminal and never installs hooks into another tool's config.
2. **The station owns execution.** Every command, git operation, file write and process runs on the station that owns the workspace. The deck never runs user code.
3. **Model help through the owner's subscription, small and optional (DL-153).** No crate calls a model API directly (test: `no_crate_calls_a_model_api_directly`). When a feature needs generated text, the station runs one short headless call of the user's own provider CLI (`claude` or `codex`) on the owner's subscription, with the smallest fitting model (Haiku 5.5 for Claude, Luna for Codex). Each such call:
    - is opt-in per feature;
    - has a token budget, and its token use shows in the usage meters;
    - is off the critical path: the feature works, plainly, without it;
    - is labeled "generated" on the board.
   Generated text never decides anything that the owner or the protocol decides. It does not approve, pick a winner, choose a permission mode or route work.
4. **Unknown is never zero.** A missing value shows as unknown, never as a guess or as 0. This covers usage, cost and limits.
5. **Credentials stay on the station.** The deck holds no provider, forge or tracker credentials. A station uses its own user's `git`, `gh`, `glab` and provider logins. A Jira or Linear token comes from the station user's environment or a mode-0600 file that `station.toml` names, and only the station reads it (DL-152). A secret never reaches the deck, the link, a read model, an argv, a log or the board (`Secret<T>`, scrubbed child env).
6. **Third-party text is data.** Text from an issue, a PR, a web page or another agent is fenced and labeled as untrusted. A spawn from such text is supervised by default, with no path to a bypass mode (PLAN §9; it is not built yet, see R3).
7. **Failure never stops work.** A deck, network or board failure never stops a session. A station restart either reattaches a shim-hosted session or marks it `lost`; it never guesses.
8. **Capability-honest UI.** The board shows a control only when the provider supports it. A refused control shows the reason.
9. **Contracts first.** Every wire shape is a versioned Rust type. A peer of a newer build keeps unknown events unchanged.
10. **Four proofs per feature.** Each feature has these four tests, and `PARITY.md` names them:
    - **fail-open:** what keeps working when it breaks;
    - **determinism:** no model call decides anything; model help (rule 3) is optional, budgeted and labeled;
    - **exposure:** no secret leaks;
    - **version resilience:** an older or newer peer degrades and does not break.
11. **People other than the owner are principals (DL-154).** A collaborator acts only inside what an owner share gives them. The board names them on every message and answer, and the audit log records them. An owner-only surface stays owner-only. Steering an agent means that the agent runs commands on the owner's machine, so a share invite says this in plain words.
12. **Artifacts are first-class (DL-155).** An artifact is an immutable, versioned file that a session or a person publishes: HTML, Markdown, an image, SVG or PDF. The board shows it natively. An HTML artifact always runs in a sandbox that cannot reach the board, its cookies or its API.

---

## 3. What v1 planned and v2 has

These v1 and v1.0 items are done in v2, often in a stronger form. They need no more work.

| v1 idea | v2 evidence |
| --- | --- |
| Contracts-first typed boundary, `schema_version` | `crates/fleetdeck-proto`, `web/src/gen`, `version_skew.rs` |
| Normalize at intake, provider-blind core | `event.rs`, `capability_matrix.rs` |
| Second provider (Codex) as first class | Native app-server adapter, ADR-002, replay fixtures |
| Per-turn checkpoints, revert only when idle | `checkpoint.rs`, `revert_run.rs`, `web/e2e/revert.spec.ts` |
| Waitable typed completions for a coordinator | `GET /api/tasks/{id}/wait`, MCP `wait_task` |
| Versioned control skill, privilege model, spawn cap | Coordinator MCP, grants with classes, `spawn_cap`, expiry (ADR-006) |
| Multi-account pinning (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) | Provider instances with `config_home` (PLAN §6.7) |
| Permission ladder | Mode ladder from native modes |
| Batch spawn, `Nx`, ticket callsigns | `web/e2e/spawn.spec.ts`, DL-144 |
| Jump palette | `web/src/app/Palette.tsx` |
| Usage meters, unknown is not zero | `UsageMeters.tsx`, `web/e2e/usage.spec.ts` |
| Chat view | Chat is the primary view; terminal and TUI handover are explicit |
| Numbered migrations with rollback | `fleetdeck-core/src/db.rs`, `station/src/upgrade.rs` |
| Performance bars with a harness | `crates/fleetdeck-cli/tests/perf.rs` (`make perf`) |
| Remote host | A station on each host; spawn has a station picker |
| Image paste, worktree removal, LAN QR, `doctor`, setup command | DL-143 … DL-151 |

---

## 4. What v2 changed or rejected on purpose

Keep these decisions. Reopen them only with a DL entry.

- **Observe floor, hooks and tmux:** removed (D3). The fail-open intent moved to rule 7, the shim host (ADR-005) and the `lost` state.
- **Codex rollout tails, `auth.json` reads and the 20 MB "tail, never slurp" rule:** not needed, because limits and items come from the protocol.
- **Editing another tool's config (`config.toml`, `hooks.json`):** never done. `fleetdeck doctor` only reads. If v2 ever writes there, it needs a consent step and an uninstall.
- **Remote control (`/rc`), `/clear` succession, Stop-hook mail:** dropped.
- **An Electron app, an agent-agnostic core, Computer Use, a hosted relay, "AI picks the winner":** rejected, as v1.0 also decided. Model help (rule 3) may summarize the candidates of a race; the owner picks.
- **Jira and Linear:** v1 cut them, because a tracker token is a stored credential. The owner reversed this (DL-152): GitHub, GitLab, Jira and Linear are all in scope, with the tracker token held only by the station (rule 5).
- **LLM-written titles:** now allowed as opt-in model help (rule 3, R10). Callsigns stay deterministic.
- **The T3 Code "do not adopt" table:** obsolete for v2. v2 now mediates (drive, event sourcing, normalization). Only its "no relay" and "no LLM calls" rows still hold.

---

## 5. Lessons that still apply

These come from Orca, T3 Code, the v0 UX plan and the v1 reviews, kept only where they still teach v2 something.

1. **Review is the main daily action, and v2 has only half of it.** Orca, T3 Code and v1 P2 all say this. A user reviews what an agent changed in total, then sends one set of notes. v2 has per-turn diffs and revert, but no whole-session diff and no notes (R1).
2. **Batch the feedback.** Line notes sent as one message give one revision pass. A drip of notes makes the agent swing (Orca). The mail outbox already delivers one framed follow-up, so only the UI is missing.
3. **A fleet must run without a human at the keyboard.** This needs workers that can escalate, completions that say "blocked" or "question", a dead worker that ends its task with a reason, and a coordinator that waits for any event (Orca, v1 P5). v2 has tasks and `wait_task`, but no escalation, no `blocked` or `question` kinds, and no dead-worker synthesis (R2).
4. **The injection boundary comes before any forge or tracker feature.** Hostile issue text plus an unsupervised spawn is a live exploit chain (v1 P3). Build the fenced envelope and its hostile fixture first (R3).
5. **Limits govern a busy day.** The useful facts are the tightest window first, the time to reset, a warning before a limit stops work, and which account has room (v1 P4, Orca) (R4).
6. **A failed asynchronous step must say why, where the user looks.** The v0 UX plan found silent clone and spawn failures. The same risk exists in v2 for a clone, a setup command or a worktree that fails after the form closes (R7).
7. **Measure before you promise; a false green is worse than red.** The v1 Bun lane ran zero tests and exited 0. v1 also found its migration size understated five times (R8).
8. **A decision is not landed until the documents that it invalidates are corrected (propagation debt).** `DECISIONS-LOG.md` is about 2,400 lines, so stale statements in `PLAN.md` are likely (R9).
9. **Pin what you read from a provider.** An old CLI that boots but speaks an older protocol is a silent break. v1 hit the same class of bug with a Node engine floor (R8).
10. **An agent can make code look more correct while it makes the code less fail-open.** The rule is green or revert, and a worker never edits a test to make it pass (v1 foundations-hardening) (R9).
11. **Do not make an empty or inert control look broken.** A disabled button says why. A first run offers a clear first action. v2 does this in the spawn form (DL-141); keep it as a rule for every new surface.
12. **Close is not kill.** In a terminal wall, "close" only detaches the view. "Kill" is a separate, confirmed action. Keep this rule when the grid is built.

---

## 6. The gaps, ranked

Each theme lists its items best first. Every item names its v2 status.

### R1. Harvest: review a finished task (highest value)

1. **Record the base of each spawn (`base_ref` and the resolved `base_oid`) at spawn time.** For an imported session, use `merge-base HEAD origin/HEAD` and label it "inferred base". Missing: a requested `base` name is kept (DL-144), but no resolved commit is. Start this first, because history accrues from the day it lands.
2. **A diff since spawn, for the whole session, including untracked files,** on the session's Changes tab and as a total on the card. Built (DL-159).
3. **Line notes in the diff view, sent as one batched follow-up** (format: `re: <file> L<n>: <note>`), kept on the screen for the next revision. Built (DL-159).
4. **Compare the winner of an `Nx` batch:** a side-by-side summary of the sibling sessions, then "keep this one" (the others go to the worktree trash). Built (DL-170).
5. **Checkpoint scope for untracked files, and garbage collection of checkpoint refs** when a worktree is purged or a session is archived. Built (DL-159; untracked files stay out of checkpoints).

### R2. Orchestration: a fleet that runs unattended

1. **`blocked` and `question` completion kinds** for tasks, beside `done` and `failed`. Built (DL-160).
2. **Dead-worker synthesis:** a task whose session becomes `lost` or `exited` without a completion ends as `blocked`, with the reason. Built (DL-160).
3. **Worker escalation:** an MCP tool through which a worker asks its coordinator a question; the question becomes a `question` completion or a mail. Built (DL-160).
4. **Wait for any event:** a long-poll over several tasks or sessions (done, blocked, question or error). Built (DL-160).
5. **Group mail addresses** `@idle` and `@<provider>`, built only when a coordinator needs them. Built (DL-160).

### R3. Integrations: GitHub, GitLab, Jira and Linear (owner decisions DL-152, DL-156)

The board works with issues and pull requests from four systems. The station does all forge and tracker I/O with its own user's credentials (rule 5). The deck relays the answers and stores only bindings: a site URL, a project key, a repository. It never stores a token.

| System | Station access | Used for |
| --- | --- | --- |
| GitHub | the user's `gh` CLI (REST and GraphQL through `gh api`) | issues, pull requests, reviews, checks |
| GitLab | the user's `glab` CLI (`glab api`) | issues, merge requests, approvals, pipelines |
| Jira Cloud | REST API v3 (`/rest/api/3/...`, issue bodies in Atlassian Document Format); site + email + API token from the station's env or a token file | issues |
| Linear | GraphQL API (`https://api.linear.app/graphql`), personal API key from the station's env or a token file; OAuth later | issues |

**Projects.** A Fleet Deck *project* is a repository on a station (station + path, or a clone URL) with optional bindings:
- a forge: the GitHub or GitLab remote, found from the repository's `origin`;
- one or more trackers: a Jira site and project key (for example `PROJ`), or a Linear team or project.

The owner creates a project on the board. A preflight checks each binding from the station ("the station can read Jira project PROJ at acme.atlassian.net"), like the Git preflight (DL-144).

1. **The injection envelope first.** Fenced, labeled untrusted text, and a forced supervised mode for every spawn from issue, PR or tracker text. The fixture is "an issue body that asks for `--dangerously-skip-permissions` still gives a supervised worker". Missing; this is the gate for everything below.
2. **The station integration service.** A `[[trackers]]` section in `station.toml` (`kind = "jira" | "linear"`, the site, and `token_env` or `token_file`; GitHub and GitLab need only their CLIs). It adds link methods to list, search and read issues and PRs, and a response cache with a short TTL. A Jira query uses JQL; a Linear query uses an issue filter. The station returns all third-party text inside the envelope. Missing.
3. **Issues on the board, with rich previews.** A project's Issues view lists issues with search and filters: status, assignee ("mine"), priority, label, sprint or cycle. A rich preview shows:
   - the rendered body (Jira ADF and Linear Markdown, with no raw HTML);
   - the status, assignee, priority, labels and estimate;
   - the comments and the linked PRs;
   - images, through a station proxy, because the deck holds no token.
   Built (DL-164).
4. **Sessions from tickets, one or a batch.** Select one or more issues, then "Start sessions". Each session:
   - is named after its ticket: the title is `PROJ-123 <issue title>`, the callsign `<animal>-PROJ-123` (exists, DL-144), and the branch `PROJ-123-<short-slug>` in its own worktree;
   - gets the issue text in the envelope as its first prompt context, together with a prompt template that the owner can edit per project;
   - keeps an `issue` link (system, key, URL) in its summary, so the card shows a ticket chip and the issue view shows its sessions.

   A batch uses the existing batch spawn (`Nx` also works per ticket). Built (DL-164).
5. **Pull and merge requests per project.** A PR/MR view with these groups: **review requested from you**, **yours** (open, draft, approved, changes requested), **pending** (waiting on review or checks), and **merged or closed** recently. Each row shows its CI checks, review state, mergeability, ticket key and age. Built (DL-164).
6. **Session ↔ PR links.** A session links to a PR when the PR's head branch is the session's branch, or when the PR names the session's ticket key. The card shows a PR chip with its state, and the PR row shows its sessions. Built (DL-164): the card shows the issue chip, and the PR row shows its sessions; a PR chip on the card is open.
7. **Review in one click.** On a PR or MR (in "review requested from you" or any row), "Start review session" makes a worktree on the PR head and starts a supervised reviewer session. The reviewer uses the user's installed review skill or the project's review prompt, and gets the PR description and the diff summary in the envelope. Built (DL-164).
8. **"Fix the broken checks":** a session gets the failing check names, the links and the log tails of a PR or MR. Built (DL-164).
9. **Writes, later:**
   - create a PR or MR from a finished session (with model-help text, R10.3);
   - post a review or comment;
   - move a Jira or Linear issue (for example to "In review" with the PR link).

   Each write needs a verb allowlist, an identity preview ("posts as @user"), one board confirm per write and an audit line, and it is never batched or automatic. Built (DL-164).
10. **GitLab details:** subgroup paths with three or more segments, and SSH and https remotes that name the same project. Partial (the clone preflight exists, DL-144).

### R4. Usage and accounts

1. **Tightest window first, with a reset countdown** ("resets in 2 h"). Built (DL-161).
2. **An 80% warning on the fleet home and on the session header,** not only inside the Usage tab. Built (DL-161).
3. **The limit of each provider instance in the spawn picker,** so the owner sees "account A resets in 15 min, use B". Built (DL-161).
4. **Assign by quota:** `assign auto` prefers the instance with the most room. Built (DL-161).
5. **Claude windows that show before a limit.** The adapter asks the CLI for the plan windows with the stream-json control request `get_usage` (after `init`, then after a turn, at most every 5 minutes), and the answer feeds the same meters as `rate_limit_event` (`claude/usage.rs`). Built (DL-161).
6. **Codex cost from a local price table, labeled "inferred"** (owner decision DL-157). Built (DL-161).

### R5. Automations

1. **Scheduled runs (all kinds, DL-157):** daily triage, nightly test fixes, PR review sweeps, dependency updates, and any owner-defined schedule. Each has a cron expression or a preset, a prompt, a repository, a station and a provider instance. A shell precheck runs on the station (never on the deck). A non-zero precheck records a "skipped" run. A missed run gets a grace period, and a run can reuse a session. The deck owns the schedule because it is always on. Built (DL-168).

### R6. Views at fifteen or more sessions

1. **Unread state per session** with a stored read cursor: "what did I miss" on a phone. Built (DL-169).
2. **Watch many sessions at once: explore another form than a terminal wall** (DL-157). The owner does not need a grid of terminals, but wants a way to watch the fleet. Candidates to explore:
   - a "live wall" of compact chat tiles (the last lines and the current tool of each session);
   - a focus mode that cycles through the sessions that need attention;
   - split panes of two to four session chats.

   Explore first, with mockups; build the one that the owner picks. Built (DL-169): the Watch page (`#/watch`) with live tiles and a focus mode; the owner can still pick another form.
3. **"No match, start a session" row** in the palette. Built (DL-163).
4. **A one-line "current focus" that the agent writes** (through MCP) and the card shows. Built (DL-165, DL-169).
5. **A merged activity stream with a channel per session or repository.** Built (DL-169).
6. **Hibernation:** stop long-idle finished sessions and resume them on open. Built (DL-169).

### R7. Honest feedback

1. **Asynchronous spawn failures** (clone, worktree, setup command) appear at once on the board, with the redacted Git or shell error, and deduplicated per session. Built (DL-163).
2. **Plan execute atomicity:** a failed start leaves the plan `proposed`; a successful one marks it `executed`. Also add a "proposer still live" note. Built (DL-163).
3. **A written rule for refused controls:** hide a control when the provider does not support it; disable it with the reason when a permission or a state blocks it. Built (DL-163).
4. **A written Esc and overlay order, with a test that every overlay blocks the board hotkeys.** Built (DL-163).

### R8. Platform and quality

1. **macOS checks on the owner's machine.** GitHub CI runs only on demand now (DL-158), so the owner runs `make check` locally, also on a macOS machine before a release. D12 promises macOS, and setup cleanup, launchd, socket paths and process groups differ there. Built (DL-162): `TESTING.md` §6 lists what to run.
2. **A provider version check (architect's decision, DL-158).** Each adapter names the oldest CLI version that its replay fixtures and live smoke cover. An older CLI is not refused: the station marks the provider "older than tested", and the spawn form and `fleetdeck doctor` show a warning. The station refuses only a version that is known to break the protocol (a short deny list). A `make` target (not CI) diffs the Codex app-server schema of a newer release against the pinned one. Built (DL-162): `version_status`, `make codex-schema-diff`.
3. **A crate-layering test** (`cargo metadata`): `fleetdeck-proto` imports no server crate, `fleetdeck-providers` does not import `fleetdeck-deck`, and the deck never matches on a provider name. Built (DL-162): `crates/fleetdeck-cli/tests/layering.rs`.
4. **A guard against a silent zero-test pass:** each lane asserts a minimum test count. Built (DL-162): `tools/check.sh`.
5. **A checkpoint latency bar:** a checkpoint adds no latency to an approval or a turn completion. Built (DL-162): a named bar in `tests/perf.rs`.
6. **A 15-session board load test.** Built (DL-162): `tests/perf.rs`.
7. **A reproducible board build** embedded in the binary (build twice, compare). Built (DL-162): `make board-repro`.

### R9. Documents and process

1. **The owner's security review at the end** (DL-158). The architect keeps a checklist of every surface that is new since the last design review, for that review: DL-143 … DL-157 and every later DL. Each feature still gets its adversary reviews while it is built. Built (DL-162): `SECURITY-CHECKLIST.md`.
2. **Proof columns in `PARITY.md`:** fail-open and exposure tests for each M3 and night-2 row. Built for the night-2 rows (DL-162); the M3 rows keep their test names without the new column.
3. **A testing rule in `TESTING.md`:** green or revert; never edit, skip or ignore a test to make it pass; a list of fail-open behaviors that must not get stricter. Built (DL-162): `TESTING.md` §7.
4. **Pay the propagation debt:** reconcile `PLAN.md` §10 (data sketch) with the real migrations, and add an "invalidates" line to each new DL entry. Built (DL-162) for `PLAN.md` §10; the "invalidates" line stays a rule for each new DL.
5. **`internals.md`:** a board route map (route, scope, mutating) and a session lifecycle diagram (lanes and effect states). Built (DL-162): `internals.md` §9 to §11.
6. **"A day on the deck":** one owner story that uses every feature once, as the script for the cutover rehearsal. Built (DL-162): `DAY-ON-THE-DECK.md`.
7. **A one-line release gate for 2.0.0:** proofs, migrations, tests, performance, platform, fallback and the security delta are all green. Built (DL-162): the gate sentence at the end of §9.

---

### R10. Model help (owner decision DL-153)

Small, cheap, optional text from the owner's own subscription (rule 3). It runs on a station with Haiku 5.5 (Claude) or Luna (Codex), and it uses a short headless CLI call with a budget. Candidates, most useful first:

1. **Session titles** from the first prompt, for sessions without a title (one call per session, about 100 output tokens).
2. **A summary of a finished session or turn** ("what did this agent do"), on the card and in "what did I miss". This works with R6.1 unread.
3. **PR and commit text** from the whole-session diff (R1.2) and the session summary, for R3.9 writes, always shown for the owner to edit.
4. **An issue digest** for the R3 issue browser: a two-line gist of a long issue. Fenced untrusted text goes in, and the digest is labeled as generated from untrusted text.
5. **A race summary** for R1.4: what each sibling session changed, side by side. The owner picks.
6. **A conflict explanation** for the conflict mail: which change of each session touched the same file.
7. **A plan gist** in the plan library.

Rules for every item:
- **Budget:** a per-call token cap and a daily cap per station.
- **Cache:** a result is cached by its input hash, so the same input never costs twice.
- **Off switch:** a global switch in the station and deck configuration.
- **Visible cost:** the token use shows in the usage meters under a "Fleet Deck" scope.
- **Failure:** a failed or skipped call leaves the plain fallback (no title, no summary), never an error state.

### R11. Shared fleet and shared sessions (owner decision DL-154)

Goal: a team works on the same sessions. A person with an invite can see and follow a session, and, with the right role, steer it and answer it, as if it were their own.

Today v2 has one human principal, the owner (a password login), plus bearer tokens with scopes (`read`, `drive`, `approve`, `admin`) and owner-only routes (DL-138). Sharing adds people.

Design sketch (recommended defaults; the adversary reviews it before it is frozen):

1. **Share objects.** The owner shares one of three things, each with a role. A share has an expiry (default 7 days) and the owner can revoke it at once.
   - **A session:** that session only, its timeline, approvals and changes.
   - **A space:** a named set of sessions, for example "the auth refactor". New sessions can join it.
   - **The board:** every session, for a trusted teammate.
2. **Roles:**
   - **Viewer:** sees the chat, the timeline, the plan, the diffs and the artifacts, live, and follows the session.
   - **Collaborator:** a viewer that can also send prompts, steers and follow-ups, interrupt, and answer the agent's questions.
   - **Approver:** a collaborator that can also answer tool approvals.

   The invite level sets the role: a collaborator invite cannot answer tool approvals, an approver invite can (DL-156). No role can raise the permission mode above the one that the owner set, change the model provider, or start a session in bypass mode.
3. **Always owner-only:** shells and the TUI handover (a shell is the machine), the setup command, worktree delete, station pairing and configuration, device links, coordinator grants, tracker and forge credentials, model-help budgets, and the creation of shares.
4. **The invite URI** is `https://<deck>/#join=<token>`. It is single-use or limited to N uses, and it expires. The person who opens it picks a display name, and the deck makes a member login: a cookie tied to that member and that share. The token travels only in the fragment, as login links do (DL-146). Members are local accounts on the deck (DL-156). A later version can add a GitHub or OIDC login, or a passkey per member.
5. **Attribution and presence:**
   - every input event carries its author (owner or member name), and the chat shows it;
   - the board shows who watches a session now;
   - approvals record who answered them;
   - the audit log has every member action.
6. **Concurrent steering and mob mode:** a shared session has a steering mode (DL-156).
   - **Open:** everyone with the collaborator role steers, and the messages queue in order, as they do today.
   - **Mob (pairing):** one driver at a time. Another person sends a "request to drive". The driver or the owner hands the composer over with one click. The others still see everything and can comment in the session's side chat (people only; the agent does not read it).
7. **Reach:** the deck must be reachable for the team, through a LAN, a tailnet (Tailscale sharing), or the owner's own tunnel. Never through a hosted relay. A self-signed certificate is a problem for guests, so the docs recommend a tailnet name or a real certificate.
8. **Exposure:** a shared session shows its code, diffs and transcript. The invite shows what the person will see. A share of a session never shows other sessions, stations, usage or credentials.
9. **The file browser for members** shows only the session's workspace (DL-156). The station hides sensitive files from members: in the browser, in search, in reads and in diffs. The default deny list is `.env*`, `*.pem`, `*.key`, `id_rsa*`, `id_ed25519*`, `.npmrc`, `.pypirc`, `.netrc`, `.git-credentials`, `credentials*.json`, `*.keystore`, `.aws/`, `.ssh/`, `.gnupg/` and `secrets*`. The owner can extend it per project. The owner still sees everything.

Owner answers (DL-156): (a) the invite level decides, so only an approver invite answers tool approvals; (b) local accounts; (c) the file browser shows the session's workspace with sensitive files hidden; (d) mob and pairing sessions have "request to drive".

### R12. Artifacts as first-class citizens (owner decision DL-155)

Goal: an HTML page that an agent makes, for example with `/explain-html`, opens inside Fleet Deck, keeps its versions, and can be shared with anyone the owner chooses. The same holds for Markdown, images, SVG and PDF.

Design sketch (recommended defaults; adversary review before freeze):

1. **What an artifact is:** an immutable file version with an id, a title, a kind (html, md, image, svg, pdf), the session that made it, its author, and its creation time. A new publish of the same artifact makes a new version; the old versions stay.
2. **How an artifact is made:**
   - (a) an agent calls a Fleet Deck MCP tool `publish_artifact(path, title)` that every session gets (not only coordinators);
   - (b) the station suggests one when a session writes an `.html` file that is meant to be opened (for example the `/explain-html` output), and the owner keeps it with one click;
   - (c) a person picks "Publish as artifact" in the Files panel.

   The station copies the bytes into its data directory, with no size cap (DL-156). The board shows the total size so the owner can clean up. An HTML page that names local files next to it (images, CSS, JS) is published with those files as one bundle. The deck keeps a copy of each shared version, so a share works while the station is off.
3. **Where it shows:**
   - an Artifacts tab per session;
   - a global Artifacts page with search, kind and session filters;
   - a chip in the chat at the turn that made it.

   Markdown renders with the chat renderer; an image, SVG or PDF shows inline.
4. **The HTML sandbox:**
   - An HTML artifact renders in an `<iframe sandbox="allow-scripts">`, without `allow-same-origin`. It comes from a separate origin, or from a `blob:` URL with an opaque origin, under a strict CSP.
   - It cannot read the board's cookies, storage or API, cannot navigate the top window, and cannot open popups without a click.
   - **No CDN allowlist (DL-156).** An HTML artifact may load anything: CDN libraries, fonts, images, local files that the page names. The team owns that risk. The opaque-origin sandbox stays, because it protects the board's login and API, not the page's content.
5. **Sharing:**
   - **A share link per artifact** (`https://<deck>/a/<id>#<token>`): read-only, one version or "always the latest", with an expiry, revocable, and recorded in the audit log.
   - **Download:** a person can download the file, for example to send it by mail.
   - **Inside a shared space:** artifacts follow the space's share (R11), so collaborators see the artifacts of a shared session.
6. **Model help (R10):** a one-line summary per artifact for the list, optional.
7. **Later:** comments on an artifact; an artifact as the input of a new session ("work from this design").

Owner answers (DL-156): (e) yes, a public share link may reach people outside the team; (f) no CDN allowlist; (g) no size cap.

## 7. Proposed sequence

All the work in this table is planned (DL-157). The steps set the order of the work, and the cut line names what moves to a later step if a step runs long.

| Step | Contents | Cut line |
| --- | --- | --- |
| **M4.0 (now, small)** | R1.1 base commit at spawn; R8.3 layering test; R8.4 zero-test guard; R9.3 testing rule; R4.1–R4.2 tightest-first and the 80% chip; R7.1 spawn-failure banner | Nothing; each item is small |
| **M4.1 Harvest** | R1.2 diff since spawn; R1.3 batched line notes; R1.5 checkpoint GC | Untracked files in the diff (label them "not in the diff") |
| **M4.2 Unattended fleet** | R2.1–R2.4 | Wait-for-any (coordinators can loop over `wait_task`) |
| **M4.3 Hardening** | R8.1 macOS check list; R8.2 provider version check; R9.2 proof columns; R9.4 propagation debt; R9.7 gate; the R9.1 checklist kept current | None |
| **M5 Integrations, read only** | R3.1 envelope and fixture; R3.2 station service (GitHub, GitLab, Jira, Linear); projects and bindings; R3.3 issues with rich previews; R3.4 sessions from tickets (one or a batch); R3.5 PR/MR view; R3.6 session ↔ PR links; R3.7 review in one click; R3.8 fix checks | R3.8 |
| **M5.1 Accounts** | R4.3 limits in the picker; R4.4 assign by quota; R4.5 Claude windows research | R4.5 |
| **M5.2 Model help** | R10.1 titles; R10.2 session summaries; the budget, cache and off switch | R10.2 |
| **M5.3 Artifacts** | R12.1–R12.5: publish (MCP tool, Files panel, the HTML suggestion), the viewer and sandbox, versions, share links | R12.2 (b) the automatic suggestion |
| **M5.4 Shared fleet** | R11.1–R11.9: shares, roles, invite URI, local member accounts, attribution, presence, audit, the member file deny list, mob mode with request to drive | Spaces (start with session and board shares) |
| **M6** | R1.4 compare the winner; R5 automations; R6.1 unread; R6.3 palette row | R5 missed-run grace |
| **Later, owner decision** | R3.9 writes (PRs, comments, issue moves); R6.2 terminal wall; R6.5 activity stream; R6.6 hibernation; R4.6 inferred cost; ACP for a third provider | — |

---

## 8. Owner decisions needed

1. **Integrations (R3):** decided (DL-152, DL-156, DL-157): all four systems, reads and writes. All of the work is planned, so the milestones only set the order of the work.
2. **Watching many sessions (R6.2):** no terminal wall. Explore another form (DL-157).
3. **Inferred Codex cost (R4.6):** yes, labeled "inferred" (DL-157).
4. **Automations (R5):** all kinds (DL-157).
5. **When to release 2.0.0:** the owner says when (DL-158). Until then, all work goes on the `v2` branch.
6. **Shared fleet (R11):** decided (DL-154, DL-156).
7. **Artifacts (R12):** decided (DL-155, DL-156).

**About decision 5.** "2.0.0" is the first v2 release that replaces v1 for daily use (`docs/v2/cutover.md`). The owner cuts it when they decide; the owner's security review comes before it (DL-158).

---

## 9. What this document does not change

This document changes no decision of `PLAN.md` or `DECISIONS-LOG.md`. Each step of §7 starts with a DL entry and, for a high-risk part (forge text, automations that start sessions, PR writes, shared access, the HTML artifact sandbox), an adversary design review, as the owner's review rules require.

**Release gate for 2.0.0 (R9.7).** The owner cuts 2.0.0 only when all of these are green: proofs (every `PARITY.md` row has its proof columns), migrations (the migration tests pass), tests (`make check` passes, with the R8.4 test-count floors), performance (the `make perf` bars pass), platform (the macOS list of `TESTING.md` §6 is done), fallback (v1 still runs and the `cutover.md` way back works) and the security delta (the owner has reviewed every row of `SECURITY-CHECKLIST.md`).
