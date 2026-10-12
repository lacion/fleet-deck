# What Fleet Deck v2 can learn from T3 Code

*Written 2026-10-11 from a source read of [pingdotgg/t3code](https://github.com/pingdotgg/t3code) at `5f7294d` (2026-10-10): `docs/internals/*`, `docs/orchestration-v2/*`, `docs/user/*`, and `packages/provider-pi`. Companion to [PLAN.md](./PLAN.md).*

The v0 lessons doc (`docs/t3code-lessons.md` on `main`, August 2026) rejected most of T3 Code, because v0 *observed* Claude Code and T3 Code *drives* agents. v2 now makes the same bet as T3 Code: drive agents through their structured protocols, with a server that owns execution and clients that render it. Most rows in the old "do not adopt" table are now our own decisions. This doc reads T3 Code again with that new frame.

---

## 1. The short version

T3 Code has solved many of the problems that our plan describes, and it documents why. The four most important lessons:

1. **Commit intent, receipt, projection, and outbox in one transaction.** The provider call happens later, in an effect worker. An acknowledgement means "the intent is durable", not "the provider did it". This is a stronger form of our command ledger (A4).
2. **Approvals and questions are durable records**, not live callbacks. They survive a restart. If the provider cannot continue them after a restart, they become `not_resumable` explicitly.
3. **A detailed capability system with a degradation policy for each missing capability.** Our capability matrix is too flat. T3 Code says what the product does when a capability is missing (for example: no native steer → interrupt and restart the turn).
4. **Replay-fixture tests.** Record real provider transcripts. Replay them through the real adapter, the real event store, and the real projections. Only the provider transport is fake.

One important difference stays: **T3 Code has no hub.** Each client connects directly to each environment (server). Fleet Deck v2 has a deck. Section 3 explains why we keep the deck.

---

## 2. What T3 Code is now

| Item | Value |
| --- | --- |
| Positioning | "Agent harness control surface". Web, Electron desktop, iOS, and Android clients |
| Providers | Claude (TypeScript Agent SDK, `@anthropic-ai/claude-agent-sdk` ^0.3.276), Codex (app-server), Cursor, Grok, OpenCode, Pi (`pi --mode rpc`), Antigravity, any ACP Registry agent |
| Stack | Effect-TS monorepo. Server ~200k lines of TypeScript (`apps/server/src`) |
| Server | An **environment**: owns providers, PTYs, Git, files, durable state (SQLite) |
| Clients | Shared `packages/client-runtime` (connection, auth, state) for web and mobile |
| Remote | Direct LAN, Tailscale, SSH (desktop launches the server), **T3 Connect** (hosted relay with Clerk identity and managed tunnels) |
| Orchestration | "v2": event-sourced, with an execution graph (thread → run → execution nodes → provider threads) |
| Agents as clients | An MCP endpoint (`/mcp`) gives agents tools such as `delegate_task`, `t3_thread_send`, `t3_thread_wait` |

---

## 3. Topology: they have no hub, we do

**T3 Code:** a client (web, desktop, or phone) keeps a catalog of environments and opens one connection to each. The hosted web app "does not proxy traffic or hold server-side pairing state". A project and its threads belong to exactly one environment. A repository identity "can correlate clones across environments, but never routes work between them".

**Fleet Deck v2:** the board connects to one deck. The deck connects to N stations.

| Concern | T3 Code (client aggregates) | Fleet Deck v2 (deck aggregates) |
| --- | --- | --- |
| Cross-machine features (mail, `assign auto`, conflict radar, a coordinator that delegates to another machine) | Not possible on the server side. Only the client sees all environments, and only while it is open | Possible. This is the reason for v2 |
| Phone reaches a machine behind NAT | Needs Tailscale or the hosted relay | The station dials the deck. The phone reaches only the deck |
| Single point of failure | None | The deck. Mitigated by doctrine rule 2 (sessions continue without the deck) |
| Number of things to secure | Each environment is exposed to clients | Only the deck is exposed to browsers. Stations accept only the paired deck |

**Decision:** keep the deck. T3 Code confirms that "the environment that owns the workspace owns execution", which is our station rule (A1). It does not have our fleet-level features, so it has no need for a hub.

**Borrow from their remote model:**

- **Identity is independent of the route.** A station keeps its ID across restarts and address changes. The deck stores an **ordered list of routes** for a station that it dials, tries them in order, and accepts a route only when the peer proves the pinned key.
- **Learned routes.** A connected station reports its LAN and tailnet addresses. The deck saves them as *hints*. Routes that the owner entered are never removed automatically.
- **No silent fallback to loopback.** A loopback address refers to a different machine when another device opens it.
- **Jittered exponential backoff, capped at 5 minutes, reset only after the link stays up.** Without jitter, every station reconnects in the same second after a deck restart.

---

## 4. Lessons that change the plan

Each row names the plan section that changes. Priority: **P1** = change before Phase 1 (all P1 rows and L4 are applied in PLAN.md draft 3); **P2** = change before the phase that builds it; **P3** = later.

| # | T3 Code lesson | Evidence | Change to PLAN.md | Pri |
| --- | --- | --- | --- | --- |
| L1 | **One transaction: events + projections + command receipt + outbox effects.** Subscribers see events only after commit. An effect worker does provider and filesystem work after the commit, then feeds results back as events. "Keep external I/O out of command decisions and the database transaction." | `docs/internals/overview.md` "Durable intent and side effects"; `EventSink.ts`, `EffectWorker.ts` | §6.4: replace "command ledger" with this pattern on the station. Command response = "accepted, committed at `station_seq` N". Provider results arrive later as events | P1 |
| L2 | **Effects tied to a lost provider process cannot replay.** Recovery "retires them before admitting new work". | `overview.md`; `ProviderRuntimeRecoveryService.ts` | §6.4 + A10: on station start, retire open effects for dead processes and record why, before new commands run | P1 |
| L3 | **Durable runtime requests.** Approvals and questions are persisted records with the provider's own option IDs. Capabilities say whether a request survives a restart (`pendingRequestsSurviveRestart`, `approvalCallbacksAreLiveOnly`). Otherwise the request becomes `not_resumable`. Answering a request is idempotent: a repeated command returns its receipt. | `provider-capability-system.md`; `providers.md` "Protocol traps" | §6.3: add `approval.expired { reason: not_resumable }`. §6.5: an open approval after a station restart is shown as expired, not as still answerable. Keep native option IDs (a display label is not always a valid reply) | P1 |
| L4 | **Codex questions arrive as notifications and are answered with a new user message**, not with an RPC response. | `providers.md` "Protocol traps" | §6.2: add an approval "response capability": `rpc_reply` or `message`. Spike S2 must cover it | P2 |
| L5 | **A grouped capability shape plus a degradation policy for each missing capability.** Examples: no active steer → interrupt + restart as a "steering replacement attempt"; no rollback → restore files, mark the provider conversation "divergent"; no structured approvals → the provider runs under its sandbox policy and the UI shows no approval controls. `terminalStatusQuality` = `strong`, `weak`, or `none`. | `docs/orchestration-v2/provider-capability-system.md` | §6.2: replace the flat matrix with groups (sessions, turns, streaming, tools, approvals, planning, subagents, checkpointing, identity). Add a "degradation" column. The UI receives typed capability results, not provider errors | P1 |
| L6 | **App IDs are primary; provider IDs are refs.** Correlation is always scoped (provider + session + thread), never global. Providers with weak or no IDs get app-allocated IDs by ordinal. | `docs/orchestration-v2/entity-ids-and-correlation.md` | §6.3, §10: add `run_id`, `node_id`, `provider_turn_id`, `request_id` as app IDs; store native IDs in a `provider_refs` object | P1 |
| L7 | **Run ≠ provider turn ≠ execution node.** Child (subagent) completion never closes the parent run. Background work and subagents can report after the parent turn ended (`hasPendingBackgroundWork`). Codex `/goal` can start several native turns in one run. | `docs/orchestration-v2/README.md` "Key invariants"; `adding-a-provider.md` "Traps" | §6.3: give tool/approval/subagent events a `node_id` + `parent_node_id`. A session with pending background work is not "idle". §7.2 status projection uses this | P1 |
| L8 | **Offer only permission modes that the provider enforces natively.** "Do not imitate a missing mode by answering approvals in T3: T3's check is weaker than the agent's own enforcement." A stored mode that a provider does not offer runs as **Supervised** (fail safe). | `adding-a-provider.md`; `RuntimePolicy.ts` | §6.2 + §11: the mode ladder shows only the modes in the session's capabilities. Unknown stored mode → most restrictive mode | P1 |
| L9 | **Pi approvals through an injected extension work in production.** T3 Code writes a Pi extension to a cache and loads it with `--extension`. The extension uses `pi.on("tool_call")` and `ctx.ui.confirm` (Supervised: ask before commands, edits, extension tools; read-only tools continue; Auto-accept edits: `edit`/`write` pass). "Auto" is not offered for Pi. A mode change restarts the Pi process and resumes the same session. | `packages/provider-pi/src/server/mcpExtensionSource.ts`; `docs/user/providers-pi.md` | Deferred by PLAN.md D14 (Pi is out of v2 scope). Keep as the design input for the later Pi package | — |
| L10 | **Driver kind ≠ provider instance.** An instance is one configuration and one account. Route work by instance, so two accounts on the same driver never share session state. Remove ambient credentials that the instance did not configure. | `docs/internals/providers.md` | §6.7 `station.toml`: replace `providers = [...]` with `[[provider_instance]]` tables (`id`, `driver`, `binary`, `env`, `config_home`). This also gives v0's "gateway routing" a clean home | P2 |
| L11 | **Status probes must have no side effects.** Opening a provider session can start MCP servers, run hooks, or open a login browser. Background probes use version/initialize only. | `providers.md` "Setup must not happen as a health-check side effect" | §6.2 `probe()`: version and auth state only, never a session | P1 |
| L12 | **Raw provider frames are rotating diagnostic logs, not a database table.** Each payload has a 64 KiB budget; large payloads become structural summaries; token deltas are filtered. | `providers.md` "Provider diagnostics"; `orchestration-v2/README.md` | §10: remove the `raw_frames` table. Use bounded rotating files per session | P1 |
| L13 | **Checkpoints use hidden Git refs.** A revert must reject *before touching files* if the provider cannot roll back its conversation. "Turn ended" and "follow-up work settled" (checkpoint, diff) are separate: a late diff must not extend the turn duration or keep the card "working". Codex opts out of `turn/diff/updated`; diffs come from checkpoints. | `overview.md` "Turn completion and checkpoints"; `providers.md` | §6.6 + Phase 6: checkpoint ordering rules; `turn.completed` and `turn.finalized` are two events | P2 |
| L14 | **Agents drive the product through an MCP endpoint**, with a credential scoped to environment + thread + provider instance + session. It expires when idle, is revoked when the session ends, and is never persisted. Tools: `delegate_task`, `task_status`, `task_cancel`, `t3_thread_list/read/send/wait/interrupt`. Tool parameters only select targets; the credential sets the limits. | `docs/orchestration-v2/orchestrator-mcp-server.md`; `environment-auth.md` | §7.1 "Coordinator API": make it an **MCP server**. Cross-machine design: the station serves a loopback MCP endpoint to its own agents and forwards calls to the deck over the link. The deck checks a per-session grant. This replaces the v0 `/fleet` command + token classes | P2 |
| L15 | **Every RPC declares its required scope.** Socket authentication does not authorize every method. Bearer clients get short-lived WebSocket tickets, so long-lived tokens stay out of URLs. Pairing grants a scope set that an exchange can narrow but never widen. Pairing secrets never appear in a read model. A pairing secret in a URL goes in the **fragment**, never the query. | `docs/internals/environment-auth.md`; `remote.md` "Hosted web is a client" | §8 + §9: one scope per board-API method and per link method; the `fdinv1:` invitation keeps its secret after `#` (already so) and the board strips it from history | P1 |
| L16 | **Capabilities, not versions, decide client behaviour.** Servers outlive several client releases. The environment descriptor advertises feature flags; old fields stay for old clients. **Persisted events must stay decodable on replay**: a new event shape can break startup of an old server. | `overview.md`; `providers.md` "Attachments and stored history" | §8.2: the deck↔station handshake exchanges feature flags, not only `proto_min/max`. §3 rule 9: add "stored events are a compatibility surface; never change an event shape in place" | P1 |
| L17 | **Client connection runtime.** One retry owner per environment. Transport health and data freshness are separate states ("connected" but "shell subscription failed" is not "reconnecting"). Cached data stays readable offline but never overwrites newer live data. Per-thread subscriptions with a cached cursor (5 idle minutes) so back navigation does not download a snapshot again. | `docs/internals/connection-runtime.md` | §11 + Phase 4: a `web/src/runtime/` module with these rules; per-session subscriptions instead of one fleet stream for timelines | P2 |
| L18 | **Safe self-update.** A stable launcher owns the runtime; the new version runs as a *trial* that must migrate, bind, and park at an activation gate before the launcher commits it. The launcher snapshots SQLite (main, WAL, shm) before the trial, so rollback needs no down-migrations. Clients correlate an update ID after reconnect, because a reconnect alone cannot tell success from rollback. | `docs/internals/server-updates.md` | Phase 7 "station drain and self-upgrade": use this design | P3 |
| L19 | **Restart continuation is a preference, off by default.** It requires matching run, provider thread, session, and native resume identity. Continuation intent goes in the outbox; stable command and message IDs prevent duplicate prompts. It rechecks for newer user work, a requested stop, or a maintenance turn (`/compact`) first. | `server-updates.md` "Recovering interrupted threads" | A10: add "auto-continue after station restart" as a per-session setting (default off), with these checks | P2 |
| L20 | **Replay-fixture testing.** Record real transcripts with recorder scripts. Replay them through the real adapter, event store, projections, and checkpoint policy. Only the provider transport, clock, random source, temp filesystem, and database are substituted. Shared scenarios: `simple`, `multi_turn`, `queued_turn`, `turn_interrupt`, `message_steering`, `provider_thread_resume`, `tool_call_*` approvals. "A test that drives the adapter with hand-written frames is a guess about the protocol." | `docs/orchestration-v2/testing-strategy.md`; `adding-a-provider.md` | §13: the testkit replays recorded transcripts, not scripted fakes only. Add a `record-<provider>` tool per adapter. Inject `Clock` and ID sources so replays are deterministic | P1 |

---

## 5. Provider traps to put in every adapter brief

T3 Code lists traps that broke more than one adapter (`docs/internals/adding-a-provider.md`). Each one needs a replay fixture in our testkit:

1. **A setting written before the provider advertises it** is accepted and ignored. Wait for the option, and treat a stored option that the session does not offer as "skipped".
2. **"Default" is a sentinel, not a model.** Never send it to the provider. Switch back to the default after a named model ran, also after resume.
3. **Stop sent before the turn started** can be ignored. Wait for the provider's own turn-started signal. Test "Stop right after send".
4. **Work that the provider starts on its own** (background commands, subagents, goals) can finish after the turn ended. Report pending background work; do not release the session as idle.
5. **Subagents outlive the run that started them.** Route their updates by subagent, not by the active run.
6. **A failed or stopped turn leaves nothing open.** Close tool calls, subagents, approvals, and questions on every terminal path: completion, stop, provider failure, process exit.
7. **Usage limits are not generic failures.** Classify them as `usage_limit` with a reset time, so the card shows "limited" and can continue later.
8. **Symlinked workspaces** (macOS `/tmp`) break agents that compare canonical paths. Test one.
9. **Unknown sign-in state is "unknown"**, not "signed in".
10. **Interrupt is a request first.** The interrupted terminal state arrives later through the turn-completed event (Codex `turn/interrupt` → `turn/completed`).

---

## 6. Evidence for the open questions in PLAN.md §15

> **Status:** the owner answered all questions on 2026-10-11 (PLAN.md D8–D14). The owner's answers differ from these recommendations on Q1 (no SDK sidecar at all; the SDK is only a reference recorder), Q4 (no import), Q6 (local network and tailnet only), and Q7 (Pi is out of v2 scope). This table stays as research evidence.

| Question | What T3 Code shows | Recommendation |
| --- | --- | --- |
| Q1 Claude: native Rust or SDK sidecar | T3 Code uses the official TypeScript Agent SDK. It is a TypeScript product, so the SDK is the natural choice there. Claude goal state is visible only through command output and Stop-hook feedback in the transcript, so the SDK does not hide everything either | Keep the S1 spike. Use the SDK in S1 as a **reference recorder**: capture the wire frames that the SDK sends and receives, and use them as replay fixtures for the native adapter. If the native adapter cannot match the recorded behaviour, choose the sidecar |
| Q3 Human shell | T3 Code keeps a terminal for humans: server-owned PTYs, history capped at 5,000 lines and 8 MiB per terminal, clients attach and reattach through the environment connection | Yes, optional, Phase 6. Use the same caps. It goes over the data channel |
| Q4 v0 data import | T3 Code v2 copies the v1 database and imports conversation text, but not runs, checkpoints, approvals, or tool history. The first new message starts a fresh provider session with a budgeted handoff | Our v0 sessions are terminal sessions, so there is nothing to continue. Import the repo catalog and plans only |
| Q6 Remote access | T3 Code needs a hosted relay (T3 Connect: Clerk, Cloudflare Worker, managed tunnels, a webhook inbox) because it has no hub. The relay "holds the signing authority for mint requests"; that is an explicit trust assumption | Keep our doctrine (no hosted relay). The deck plus "station dials deck" gives the same reach for stations behind NAT. The owner still needs a route to the deck (LAN, Tailscale, reverse proxy, Coder) |
| Q7 Pi approvals | Proven: an injected Pi extension with `tool_call` + `ctx.ui.confirm` (L9) | Yes. Offer Supervised, Auto-accept edits, Full access. Do not offer Auto |

---

## 7. Ideas to keep for later (not in the MVP)

| Idea | T3 Code source | Why later |
| --- | --- | --- |
| **Provider switching inside one session** with a *portable handoff*: a budgeted selection of real messages (not an agent-written summary), with references to omitted history that the agent can fetch through the MCP thread-reading tool | `docs/user/portable-handoffs.md`, `orchestration-v2/provider-switching-and-context.md` | The same mechanism can **move a session to another station**, which is a strong fleet feature |
| **Fork** with lineage recorded first; native fork or portable handoff is chosen when the first run starts | `orchestration-v2/thread-lineage-and-context-transfer.md` | Needs the handoff mechanism |
| **Thread settlement**: the server archives a thread after its PR merges or after inactivity, without a connected client | `overview.md` | Useful after the PR features exist |
| **MCP for outside agents** (agents that we did not start) with OAuth + PKCE and a read-only or runtime-mode-ceiling grant | `environment-auth.md` "MCP clients are a separate audience" | After L14 works for our own sessions |
| **Signed asset URLs** bound to one file and its device/inode identity | `environment-auth.md` | When the board shows images or HTML that agents write |
| **Drainable workers** in tests: wait until the queue *and* the current item finish | `overview.md` | Use the idea in `fleetdeck-testkit` from the start; it costs little |

---

## 8. What not to adopt

| T3 Code choice | Reason |
| --- | --- |
| Client-side aggregation of environments, no hub | Fleet-level features need a server that sees all stations (section 3) |
| Hosted relay (T3 Connect, Clerk, Cloudflare) | Doctrine rule 5: no phone-home, no hosted relay |
| Effect-TS and its service architecture | We write Rust. Keep the *ideas* (injected clock and random sources, typed services), not the framework |
| Electron desktop and native mobile apps | A responsive web board is enough for one owner. A PWA can come later |
| Model calls in the product (thread titles, PR text, summaries) | Doctrine rule 4: no model calls in the core |
| Six forge integrations, the device panel, voice input, browser import | Out of scope for a fleet control deck |
| The full execution graph with nested checkpoint scopes on day one | Take the IDs and the parent/child rule (L6, L7). Add nested checkpoint scopes only if subagent rollback becomes a need |

---

## 9. Proposed next step

Apply the P1 rows (L1, L2, L3, L5, L6, L7, L8, L11, L12, L15, L16, L20) to PLAN.md as a new draft, and record the P2 and P3 rows in the phase where each one is built. The L1 and L14 changes alter the station's internal design and the coordinator design, so the advisor reviews them before the plan changes.
