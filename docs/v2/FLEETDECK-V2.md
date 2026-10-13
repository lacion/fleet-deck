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
3. **No model calls in the core.** The deck and the station make no LLM calls. The agents are the intelligence. Test: `no_crate_calls_a_model_api_directly`.
4. **Unknown is never zero.** A missing value shows as unknown, never as a guess or as 0. This covers usage, cost and limits.
5. **Credentials stay on the station.** The deck holds no provider, forge or tracker credentials. A station uses its own user's `git`, `gh`, `glab` and provider logins. A Jira or Linear token comes from the station user's environment or a mode-0600 file that `station.toml` names, and only the station reads it (DL-152). A secret never reaches the deck, the link, a read model, an argv, a log or the board (`Secret<T>`, scrubbed child env).
6. **Third-party text is data.** Text from an issue, a PR, a web page or another agent is fenced and labeled as untrusted. A spawn from such text is supervised by default, with no path to a bypass mode (PLAN §9; it is not built yet, see R3).
7. **Failure never stops work.** A deck, network or board failure never stops a session. A station restart either reattaches a shim-hosted session or marks it `lost`; it never guesses.
8. **Capability-honest UI.** The board shows a control only when the provider supports it. A refused control shows the reason.
9. **Contracts first.** Every wire shape is a versioned Rust type. A peer of a newer build keeps unknown events unchanged.
10. **Four proofs per feature.** Each feature has these four tests, and `PARITY.md` names them:
    - **fail-open:** what keeps working when it breaks;
    - **determinism:** no model calls;
    - **exposure:** no secret leaks;
    - **version resilience:** an older or newer peer degrades and does not break.

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
- **An Electron app, an agent-agnostic core, Computer Use, a hosted relay, "AI picks the winner":** rejected, as v1.0 also decided.
- **Jira and Linear:** v1 cut them, because a tracker token is a stored credential. The owner reversed this (DL-152): GitHub, GitLab, Jira and Linear are all in scope, with the tracker token held only by the station (rule 5).
- **LLM-written titles:** not built. Callsigns are deterministic.
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
2. **A diff since spawn, for the whole session, including untracked files,** on the session's Changes tab and as a total on the card. Missing: only per-turn diffs exist, and checkpoints cover tracked files only (`SCOPE_TRACKED`).
3. **Line notes in the diff view, sent as one batched follow-up** (format: `re: <file> L<n>: <note>`), kept on the screen for the next revision. Missing: the transport exists (the mail outbox), the UI does not.
4. **Compare the winner of an `Nx` batch:** a side-by-side summary of the sibling sessions, then "keep this one" (the others go to the worktree trash). Missing: batch, diff and trash exist (DL-150). The choice stays with the owner, never a model.
5. **Checkpoint scope for untracked files, and garbage collection of checkpoint refs** when a worktree is purged or a session is archived. Partial: hidden refs grow without a limit in a long-lived repository.

### R2. Orchestration: a fleet that runs unattended

1. **`blocked` and `question` completion kinds** for tasks, beside `done` and `failed`. Missing.
2. **Dead-worker synthesis:** a task whose session becomes `lost` or `exited` without a completion ends as `blocked`, with the reason. Missing (no task state for this in `orchestration.rs`).
3. **Worker escalation:** an MCP tool through which a worker asks its coordinator a question; the question becomes a `question` completion or a mail. Missing (ADR-006 §6.3: coordinators answer no approvals in M3).
4. **Wait for any event:** a long-poll over several tasks or sessions (done, blocked, question or error). Partial: `wait_task` waits for one task.
5. **Group mail addresses** `@idle` and `@<provider>`, built only when a coordinator needs them. Partial.

### R3. Integrations: GitHub, GitLab, Jira and Linear (owner decision DL-152)

The board works with issues and pull requests from four systems. The station does all forge and tracker I/O, with its own user's credentials (rule 5). The deck relays the station's answers and holds no token.

| System | Station access | Issues | Pull or merge requests |
| --- | --- | --- | --- |
| GitHub | the user's `gh` CLI | yes | yes |
| GitLab | the user's `glab` CLI | yes | yes |
| Jira (Cloud) | REST API v3, site + email + API token from the station's env or a token file | yes | no (links to GitHub or GitLab PRs only) |
| Linear | GraphQL API, personal API key from the station's env or a token file | yes | no (links only) |

1. **The injection envelope first:** fenced, labeled untrusted text, a forced supervised mode for every spawn from issue, PR or tracker text, and the fixture "an issue body that asks for `--dangerously-skip-permissions` still gives a supervised worker". Missing; this is the gate for everything below.
2. **The station's integration service:** a `trackers` section in `station.toml` (`[[trackers]]` with `kind = "jira" | "linear"`, the site, and `token_env` or `token_file`; GitHub and GitLab need only their CLIs). Link methods to list, search and read issues; the station returns the text inside the envelope. A preflight like the Git one (DL-144) says "the station can read Jira at <site>". Missing.
3. **A read-only issue browser on the board and spawn from issues:** one worker per selected issue from any of the four systems. The ticket key becomes the callsign (`otter-PROJ-123`, which exists) and the branch name. Missing.
4. **"Fix the broken checks":** hand an agent the failing CI check names and links of a PR (GitHub and GitLab). Missing.
5. **A PR reviewer:** point at a PR URL; the station checks the branch out in a worktree and starts a reviewer session with the user's installed review skill. Missing.
6. **Writes, later:** create a PR, post a review or comment, and move a Jira or Linear issue (for example to "In review" with the PR link). Each needs a verb allowlist, an identity preview ("posts as @user"), one board confirm per write, an audit line, and it is never batched or automatic. Missing. Stopping rule: build the writes only with the full security set; otherwise stay read-only.
7. **GitLab details:** subgroup paths with three or more segments, and SSH and https remotes that name the same project. Partial (the clone preflight exists, DL-144).

### R4. Usage and accounts

1. **Tightest window first, with a reset countdown** ("resets in 2 h"). Partial: meters are sorted by key (`web/src/board/usage.ts`).
2. **An 80% warning on the fleet home and on the session header,** not only inside the Usage tab. Partial: the bar changes color at 80%.
3. **The limit of each provider instance in the spawn picker,** so the owner sees "account A resets in 15 min, use B". Missing; the data is already on the board.
4. **Assign by quota:** `assign auto` prefers the instance with the most room. Missing.
5. **Claude windows that show before a limit.** Partial: the adapter maps `rate_limit_event` (`claude/map.rs`), but the CLI sends it mostly near a limit, so the meters are often empty. Research whether `/usage` or another protocol source gives the windows earlier.
6. **Codex cost from a local price table, labeled "inferred".** Partial (Codex reports tokens only). Optional: it conflicts in spirit with rule 4, unless the label is clear.

### R5. Automations

1. **Scheduled runs:** a cron expression or a preset, a prompt, a repository, a station and a provider instance. A shell precheck runs on the station (never on the deck). A non-zero precheck records a "skipped" run. A missed run gets a grace period, and a run can reuse a session. The deck owns the schedule because it is always on. Missing.

### R6. Views at fifteen or more sessions

1. **Unread state per session** with a stored read cursor: "what did I miss" on a phone. Missing.
2. **A terminal wall (grid) with view-only close.** Deferred (DL-131); needs an owner decision.
3. **"No match, start a session" row** in the palette. Missing.
4. **A one-line "current focus" that the agent writes** (through MCP) and the card shows. Partial (the current tool and the title exist).
5. **A merged activity stream with a channel per session or repository.** Missing; build it after a 15-session load test (v1 called it "a real subsystem").
6. **Hibernation:** stop long-idle finished sessions and resume them on open. Missing; native resume makes it cheap. Measure memory first.

### R7. Honest feedback

1. **Asynchronous spawn failures** (clone, worktree, setup command) appear at once on the board, with the redacted Git or shell error, and deduplicated per session. Partial: the failure ends the session with an error event; a visible banner and the "starting" state on the card need a check.
2. **Plan execute atomicity:** a failed start leaves the plan `proposed`; a successful one marks it `executed`. Also add a "proposer still live" note. Partial (`PlanLibrary.tsx` marks in a second call).
3. **A written rule for refused controls:** hide a control when the provider does not support it; disable it with the reason when a permission or a state blocks it. Partial.
4. **A written Esc and overlay order, with a test that every overlay blocks the board hotkeys.** Partial.

### R8. Platform and quality

1. **A macOS CI lane.** Missing; D12 promises macOS, but CI is Ubuntu only. Setup cleanup, launchd, socket paths and process groups differ on macOS.
2. **A minimum CLI version per provider, and a CI job that diffs the Codex app-server schema of a newer release.** Deferred (DL-131). This prevents silent protocol drift.
3. **A crate-layering test** (`cargo metadata`): `fleetdeck-proto` imports no server crate, `fleetdeck-providers` does not import `fleetdeck-deck`, and the deck never matches on a provider name. Missing; it is cheap.
4. **A guard against a silent zero-test pass:** each lane asserts a minimum test count. Missing.
5. **A checkpoint latency bar:** a checkpoint adds no latency to an approval or a turn completion. Missing as a named bar.
6. **A 15-session board load test.** Missing.
7. **A reproducible board build** embedded in the binary (build twice, compare). Missing.

### R9. Documents and process

1. **A security delta review of the surfaces added after the last design review:** DL-143 … DL-151 (inspection, preflight, conflict mail, device link, image paste, setup command, archive, trash, models), as one checklist. Partial: each part had adversary reviews, but no final delta pass exists.
2. **Proof columns in `PARITY.md`:** fail-open and exposure tests for each M3 and night-2 row. Partial.
3. **A testing rule in `TESTING.md`:** green or revert; never edit, skip or ignore a test to make it pass; a list of fail-open behaviors that must not get stricter. Missing.
4. **Pay the propagation debt:** reconcile `PLAN.md` §10 (data sketch) with the real migrations, and add an "invalidates" line to each new DL entry. Missing.
5. **`internals.md`:** a board route map (route, scope, mutating) and a session lifecycle diagram (lanes and effect states). Partial.
6. **"A day on the deck":** one owner story that uses every feature once, as the script for the cutover rehearsal. Missing.
7. **A one-line release gate for 2.0.0:** proofs, migrations, tests, performance, platform, fallback and the security delta are all green. Missing.

---

## 7. Proposed sequence

Each step has its cut line: the part that drops first if time runs short.

| Step | Contents | Cut line |
| --- | --- | --- |
| **M4.0 (now, small)** | R1.1 base commit at spawn; R8.3 layering test; R8.4 zero-test guard; R9.3 testing rule; R4.1–R4.2 tightest-first and the 80% chip; R7.1 spawn-failure banner | Nothing; each item is small |
| **M4.1 Harvest** | R1.2 diff since spawn; R1.3 batched line notes; R1.5 checkpoint GC | Untracked files in the diff (label them "not in the diff") |
| **M4.2 Unattended fleet** | R2.1–R2.4 | Wait-for-any (coordinators can loop over `wait_task`) |
| **M4.3 Release hardening** | R8.1 macOS lane; R8.2 version gate; R9.1 security delta; R9.2 proof columns; R9.4 propagation debt; R9.7 gate | None: this is the 2.0.0 gate |
| **M5 Integrations, read only** | R3.1 envelope and fixture; R3.2 station service (GitHub, GitLab, Jira, Linear); R3.3 issue browser and spawn; R3.5 PR reviewer; R3.4 fix checks | R3.4 |
| **M5.1 Accounts** | R4.3 limits in the picker; R4.4 assign by quota; R4.5 Claude windows research | R4.5 |
| **M6** | R1.4 compare the winner; R5 automations; R6.1 unread; R6.3 palette row | R5 missed-run grace |
| **Later, owner decision** | R3.6 writes (PRs, comments, issue moves); R6.2 terminal wall; R6.5 activity stream; R6.6 hibernation; R4.6 inferred cost; ACP for a third provider | — |

---

## 8. Owner decisions needed

1. **Integrations (R3):** decided (DL-152): GitHub, GitLab, Jira and Linear are in scope. Still open: the order of the four, and whether the writes (R3.6) follow in the same milestone.
2. **Terminal wall (R6.2):** still wanted, now that the dock and the TUI handover exist?
3. **Inferred Codex cost (R4.6):** acceptable when it is labeled "inferred", or keep "unknown"?
4. **Automations (R5):** which first schedules matter (daily triage, nightly test fix, PR review)? They decide the precheck design.
5. **The 2.0.0 cut:** does M4.3 come before or after M4.1 and M4.2?

---

## 9. What this document does not change

This document changes no decision of `PLAN.md` or `DECISIONS-LOG.md`. Each step of §7 starts with a DL entry and, for a high-risk part (forge text, automations that start sessions, PR writes), an adversary design review, as the owner's review rules require.
