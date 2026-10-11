# Fleet Deck v2 — plan of record (draft 3)

*Written 2026-10-11 against v0.23.6 (`main` @ `a9fb3a2`). Target branch: `v2` (worktree `../fleet-deck-worktrees/v2`). Status: DRAFT 3 — the owner answered the §15 questions (D8–D14). The P1 lessons (and L4) from [t3code-lessons.md](./t3code-lessons.md) are applied: L1, L2, L3, L4, L5, L6, L7, L8, L11, L12, L15, L16, L20. P2 lessons are applied before the phase that builds them.*

---

## 1. Summary

Fleet Deck v2 is a new version, written in Rust. It changes the product from "one daemon that observes the Claude Code sessions on one machine" to "one control deck that drives agent sessions on many machines".

v2 has two server components and one web client:

| Component | What it is | How many |
| --- | --- | --- |
| **deck** | The hub. It serves the web board, keeps fleet-wide state (mail, plans, routing, tasks), and sends commands to stations. | 1 per owner |
| **station** | The worker. It runs on each machine that does agent work. It starts and drives Claude Code and Codex sessions, and it owns their worktrees and files. | N per owner |
| **board** | The web client (React + TypeScript). It talks only to the deck. | Any number of browser tabs |

One binary, `fleetdeck`, contains all components:

```
fleetdeck up                 # deck + a local station (single-machine default)
fleetdeck deck               # deck only
fleetdeck station            # station only
fleetdeck deck invite        # make a pairing invitation for a station
fleetdeck station join <inv> # pair this station with a deck
```

---

## 2. Decision record

### 2.1 Owner decisions (2026-10-11)

| # | Decision | Value |
| --- | --- | --- |
| D1 | Language | Rust for deck, station, and CLI |
| D2 | Component names | **deck** (hub + UI) and **station** (runs sessions). One binary `fleetdeck` with subcommands |
| D3 | Session model | **Drive only.** Each agent is driven through its structured protocol. No tmux, no hooks, no terminal scraping |
| D4 | Network topology | **Both directions.** A station can dial the deck, or the deck can dial a station |
| D5 | Web client | **Keep React + TypeScript.** TS types are generated from the Rust types |
| D6 | Users | **One owner, many machines.** The data model keeps an `owner_id` so that teams can come later |
| D7 | Repository | Same repository, long-lived branch `v2` in its own worktree. Merge to `main` when v2 is complete and fully tested |
| D8 | Claude adapter | **Native Rust only.** The official TypeScript Agent SDK is a reference: we read its source and record its wire frames as test fixtures. No TypeScript sidecar, also not as a fallback |
| D9 | MVP durability | A10 accepted: a station restart can stop sessions until Phase 7; recovery uses the provider's native resume |
| D10 | Human shell | **Yes.** A PTY shell in a worktree or repo, opened from the board, for the owner (not for agents). Phase 6 |
| D11 | v0 data | **No import.** v2 starts with empty databases |
| D12 | Platforms | **Linux and macOS only** for deck and station. Windows is not planned |
| D13 | Network | **Local network and Tailscale tailnet only.** No public exposure, no reverse proxy, no Coder, no hosted relay |
| D14 | Pi | **Not in v2 scope.** Pi comes later as a dedicated Pi package with full integration. The adapter trait must allow it |

### 2.2 Architect decisions (with reasons)

| # | Decision | Reason |
| --- | --- | --- |
| A1 | The **station is the source of truth for its sessions**. The **deck is the source of truth for fleet intent** (mail, plans, tasks, routing, settings). | Sessions must continue when the deck or the network is down. The machine that runs the work must keep its history. |
| A2 | **A canonical event vocabulary** plus **one adapter per provider**. Status is a projection of events. | This is the strongest v0 lesson: normalize at intake, keep the core provider-blind, derive status instead of trusting self-reports. |
| A3 | Adapters declare **capabilities**. The UI shows only the operations that a provider supports. | `steer`, `fork`, `plan`, and `approve` do not mean the same thing across Claude and Codex (and later Pi). Do not pretend parity. |
| A4 | **Every mutating command has a `command_id`**. The station decides a command and commits, in **one SQLite transaction**, the resulting events, the projections, the command receipt, and the outbox effects. An effect worker does the provider and filesystem I/O after the commit and reports the results as new events (§6.4, T3 lesson L1). A duplicate `command_id` returns the stored receipt. | A link can drop between "executed" and "acknowledged". An event log alone does not make commands exactly-once. External I/O inside a decision or a transaction makes recovery ambiguous. Never auto-retry an ambiguous spawn or prompt with a new id. |
| A5 | **Deck-owned intent uses an outbox** with station receipts (mail, task assignment). | Replaying session events cannot recover deck-to-station deliveries. |
| A6 | **Transport direction does not give authority.** Roles (deck / station) are bound to identities at pairing time. | Both directions are supported, so the dialer cannot be trusted to define who is who. |
| A7 | **Two WebSocket channels per link:** `control` (small frames, high priority) and `data` (multiplexed byte streams with credit flow control). | A large file transfer must never delay an approval or an interrupt. WebSocket over TLS is one well-known transport for both hops (board and link); QUIC gives no benefit on a LAN or a tailnet. |
| A8 | **Claude adapter: native Rust implementation of the stream-json control protocol (owner decision D8).** The official TypeScript Agent SDK is the behaviour reference. Spike S1 runs the SDK through a recording proxy and saves its frames as replay fixtures; the Rust adapter must pass the same fixtures. | Both official SDKs (TypeScript and Python) are wrappers over this same CLI protocol (`claude --input-format stream-json --output-format stream-json`), so it is the de-facto contract. A native adapter keeps one binary and no JS runtime on stations. **The advisor preferred an SDK sidecar first; the owner chose no sidecar.** Risk: a protocol change reaches the SDK before it reaches us. Control: the nightly live smoke test (§13) and the SDK changelog watch; a gap is fixed in Rust or the capability is marked unsupported. |
| A9 | **No session shim in the MVP.** Design the `SessionHost` boundary now; build the shim in Phase 7. | A shim is a full supervisor (child lifecycle, output drain, reconnect ownership, upgrade compatibility). The MVP guarantee is explicit (see A10). |
| A10 | **MVP durability guarantee:** "A deck or network failure never stops a session. A station failure can stop sessions; recovery uses the provider's native resume, which can lose in-flight work and open approvals." The UI shows "resumed" differently from "reattached". On start, the station first retires every open effect that belongs to a dead provider process and records the reason; only then does it admit new commands (L2). Open approvals of a dead process become `approval.expired { reason: not_resumable }` (L3). | Honest scope. Phase 7 upgrades this to "a station restart reattaches live sessions". An effect tied to a lost process cannot replay safely. |
| A11 | Stations and deck: Linux and macOS (WSL2 counts as Linux). Windows is not planned (D12). | Unix sockets, process groups, and the shim design are Unix-first. |
| A12 | Git operations shell out to the `git` CLI. | Same as v0. Worktree behaviour and auth helpers must match what the user's own `git` does. |
| A13 | Board login is **required by default**, also on loopback. `fleetdeck open` prints a one-time login URL from a local owner-only file. | The v0 review flagged tokenless loopback as a P0 risk once agents can call the API. |

---

## 3. Doctrine for v2

v0 had five rules. v2 changes two of them (drive, not observe; many machines, not one) and keeps the spirit of the others.

1. **Drive through native protocols.** Use each agent's structured protocol. Never scrape a terminal.
2. **Work never depends on the deck.** If the deck, the board, or the network fails, running sessions continue. Approvals follow the session's offline policy.
3. **The machine that runs the work owns its history.** The station keeps the session event log. The deck keeps a synced read model.
4. **No model calls in the core.** Routing, status, conflicts, and mail are deterministic code. Agents are the only intelligence.
5. **No phone-home, no hosted relay, no public exposure.** The owner runs every component. The deck and stations are reachable only on the local network (loopback, LAN) and on the owner's Tailscale tailnet (D13). The deck and the station refuse to listen on a public address.
6. **Credentials stay on the machine that uses them.** Provider logins stay on the station. Nothing secret goes into the read model, argv, or logs.
7. **Every mutation is idempotent and audited.** Command ids, command receipts on the station, an audit log on the deck.
8. **Capabilities, not parity pretense.** A card shows exactly what its provider supports.
9. **Contracts first.** The Rust protocol crate is the single source of truth. TypeScript types are generated from it. Every wire shape carries a protocol version. **Stored events are a compatibility surface:** never change an event shape in place; add a new kind or an optional field, and keep old events decodable on replay (L16).

---

## 4. What we learn from v0

v0 is ~30k lines of TypeScript, 44 releases, and a large body of reviews (`docs/v1/*`, `docs/*-lessons.md`, `ts-migration-bugs.md`). These are the lessons that shape v2.

### 4.1 Keep

| v0 lesson | Where it came from | How v2 uses it |
| --- | --- | --- |
| Status is a projection of events, not a self-report | `src/daemon/derive.ts`, `src/daemon/events.ts` | Status columns are derived from canonical events on the deck |
| Normalize at intake, keep the core provider-blind | `docs/v1/architecture.md` | Canonical events + provider adapters (A2) |
| Contracts first, one source of truth, `schema_version` on every wire shape | `contracts/`, F1a | `fleetdeck-proto` crate + generated TS + version negotiation at link handshake |
| Snapshot + resync after reconnect | `board/src/fleetConnection.ts`, `/state` | Cursor-based resync on both hops: station→deck and deck→board |
| Exact process identity before trust or kill; never signal a foreign process | `src/daemon/run-nonce.ts`, `src/daemon/takeover.ts` | Station and deck identity keys; single-instance lock; no PID-based guesses |
| Single-flight destructive actions; visible rejection | board actions, `src/daemon/spawns.ts` | Command ids + receipts (A4); the board disables a pending action |
| Credential hygiene: scrub origin URLs, keep tokens off `/state` and argv, scrub child env | `src/daemon/env-scrub.ts`, `src/daemon/repo-identity.ts` | Same rules, enforced by types (`Secret<T>` wrapper that does not implement `Serialize`) |
| Numbered, transactional migrations with `PRAGMA user_version` | `docs/v1/architecture.md` (v0 had ~30 ad-hoc `ALTER`s) | `rusqlite_migration` from day one, on both databases |
| No model calls in the core | doctrine | Doctrine rule 4 |
| Callsigns, tickets from branch names, worktree naming `<repo>--fd-<callsign>` | `src/daemon/tickets.ts`, `src/daemon/worktrees.ts` | Same conventions |
| Batch spawn with `Nx` prefix, forced worktree isolation | `board/src/components/SpawnForm.tsx`, `src/daemon/spawns.ts` | Same feature, plus a station picker |
| Conflict radar | `src/daemon/events.ts` | Same feature, now across stations (repo grouping is a heuristic, see §6.6) |
| Fake agents and test seams for deterministic tests | `src/daemon/test-seam.ts` | `fleetdeck-testkit` with one fake agent per protocol, plus live smoke tests against pinned CLI versions |
| Provider version gate | `compatibility.json` | Per-provider min version + capability probe at station start |

### 4.2 Change

| v0 pain | Evidence | v2 answer |
| --- | --- | --- |
| tmux is the largest source of races and wedges | "tmux" appears 66 times in `CHANGELOG.md`, "race" 15, "wedge" 7 | Drive only (D3). Agents are child processes with stdio protocols |
| Hooks only observe; approvals are display-only on the board | `src/daemon/questions.ts` (1,570 lines of hold/re-arm logic) | Approvals are protocol requests that the board answers directly |
| Mail is pasted into a pane at a turn boundary | `src/daemon/mail.ts`, `src/daemon/paste.ts` | Mail is delivered as a native follow-up or steer message |
| Large modules (`spawns.ts` 3,477 lines, `http.ts` 2,546 lines) | `src/daemon/` | Small crates and modules; target < 800 lines per module |
| Full `/state` snapshot broadcast to every tab | `src/daemon/snapshot.ts`, `src/daemon/http.ts` | Versioned snapshot on connect, then deltas |
| Plugin cache and version-bump distribution trap | README "How updates actually reach you" | A normal binary release (`cargo-dist`: shell installer, Homebrew, GitHub releases). No Claude Code plugin is needed |
| One machine only | doctrine (loopback) | deck + N stations (the reason for v2) |

### 4.3 Drop

| v0 feature | Reason |
| --- | --- |
| Hooks and the hook shims | Drive only |
| tmux panes, the xterm.js agent terminal, `tmux -C` bridge | No TUI to show. The session view renders the structured stream |
| Adopt / takeover of a running terminal session | A running TUI cannot be driven. **Replacement:** "Import" resumes an existing CLI session id as a driven session |
| claude.ai remote control (`/rc`) | The deck itself is the remote control |
| mDNS "share the board" | Replaced by station discovery on the LAN (a discovery result is a suggestion, never trust) |
| Coder support, trusted origins and proxy auth for reverse proxies (`docs/CODER.md`) | D13: local network and tailnet only |
| Any import of v0 data | D11: v2 starts with empty databases |

---

## 5. Architecture

```
        browser tabs (board: React + TS)
                 │  HTTPS + WS  (login cookie, CSRF, Origin check)
                 ▼
 ┌──────────────────────────────── deck ────────────────────────────────┐
 │ board API · auth · fleet services (mail, plans, tasks, routing,       │
 │ conflicts, usage) · read model · outbox · audit · link manager        │
 │ SQLite: deck.db                                                       │
 └───────▲───────────────────────▲────────────────────────▲──────────────┘
         │ link (mTLS, WS ×2)    │ link                   │ link
         │ station dialed deck   │ deck dialed station    │ station dialed deck
 ┌───────┴──────┐        ┌───────┴──────┐         ┌───────┴──────┐
 │ station A    │        │ station B    │         │ station C    │
 │ (laptop,NAT) │        │ (LAN box)    │         │ (tailnet box)│
 │ session host │        │              │         │              │
 │ adapters ────┼─ stdio ─▶ claude / codex app-server
 │ shells (PTY) │        │              │         │              │
 │ event log    │        │              │         │              │
 │receipts · git│        │              │         │              │
 │ station.db   │        │              │         │              │
 └──────────────┘        └──────────────┘         └──────────────┘
```

### 5.1 Responsibilities

| Concern | Station | Deck |
| --- | --- | --- |
| Agent processes | Owns them (start, drive, stop, resume) | Sends commands |
| Session event log | **Source of truth** | Synced read model |
| Command receipts | **Source of truth** for command outcomes | Keeps pending commands + audit |
| Worktrees, files, diffs, checkpoints | Owns them | Proxies them to the board |
| Provider credentials and config homes | Owns them, never sends them | Never sees them |
| Mail, plans, tasks/completions, routing, settings | Receives deliveries | **Source of truth** (outbox) |
| Conflict radar | Reports touched files | Correlates across stations |
| Callsigns | — | Assigns them (fleet-unique) |
| Board, auth, audit | — | Owns them |

---

## 6. Station design

### 6.1 Internal layers

```
link client/server ─▶ command router ─▶ decide + commit (events, receipt, effects) ─▶ effect worker
                                         │
                       ┌─────────────────┼──────────────────┐
                       ▼                 ▼                  ▼
                 session manager    workspace service   policy
                       │            (git, files, diff,  (allowed roots,
                       ▼             checkpoints)        providers, caps)
                 SessionHost (trait)
                   ├─ DirectChildHost   (MVP: agent is a child of the station)
                   └─ ShimHost          (Phase 7: agent survives station restart)
                       │
                 ProviderAdapter (trait)
                   ├─ claude   (stream-json control protocol)
                   ├─ codex    (app-server JSON-RPC)
                   ├─ pi       (later, D14: dedicated Pi package)
                   └─ acp      (later: Gemini, Cursor, other ACP agents)

                 shell service (PTY shells for the owner, D10; not a provider)
                       │
                 canonical events ─▶ event log (station.db) ─▶ link
```

### 6.2 The provider adapter

```rust
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    fn kind(&self) -> ProviderKind;
    /// Probe the installed CLI: version and auth state only. A probe never opens a
    /// provider session (that can start MCP servers, run hooks, or open a login
    /// browser). Supported operations come from `capabilities()` of a live session (L11).
    async fn probe(&self) -> Result<ProviderProbe>;
    async fn start(&self, spec: SessionSpec, sink: EventSink) -> Result<Box<dyn LiveSession>>;
    async fn resume(&self, native_id: &NativeSessionId, spec: SessionSpec, sink: EventSink)
        -> Result<Box<dyn LiveSession>>;
}

#[async_trait]
pub trait LiveSession: Send {
    fn capabilities(&self) -> &Capabilities;
    async fn prompt(&mut self, input: UserInput) -> Result<()>;
    async fn steer(&mut self, input: UserInput) -> Result<()>;        // Unsupported if !caps.steer
    async fn follow_up(&mut self, input: UserInput) -> Result<()>;
    async fn interrupt(&mut self) -> Result<()>;
    /// `decision` carries the provider's own option id (a display label is not always a
    /// valid reply). Answering twice returns the first receipt (L3).
    async fn answer(&mut self, req: RequestId, decision: ApprovalDecision) -> Result<()>;
    async fn set_mode(&mut self, mode: PermissionMode) -> Result<()>;
    async fn set_model(&mut self, model: ModelId) -> Result<()>;
    async fn fork(&mut self) -> Result<NativeSessionId>;
    async fn stop(&mut self, how: StopMode) -> Result<()>;
}
```

Every operation that a provider does not support returns a typed `Unsupported { op, degradation }` result; the board receives typed capability results, never raw provider errors (L5).

**Raw frames (L12).** The adapter writes raw provider frames to a bounded rotating diagnostic log per session (files, not a database table). Each payload has a 64 KiB budget; a larger payload becomes a structural summary; token deltas are filtered out. A canonical event can carry a `raw_ref` (file + offset) for debugging a mapping error.

**Permission modes (L8).** A session offers only the modes that its provider enforces natively. The station never imitates a missing mode by answering approvals itself. A stored mode that the provider does not offer runs as the most restrictive mode (Supervised).

**Capability matrix (to verify in Phase 0).** Capabilities are grouped. Each row has a degradation policy for a provider that does not support it. Phase 0 replaces every "verify" with a verified value and adds the evidence (fixture or test name) in the last column.

| Group | Capability | Claude (stream-json) | Codex (app-server) | Degradation when missing | Evidence |
| --- | --- | --- | --- | --- | --- |
| sessions | Resume | `--resume <session_id>`: same session id, context kept | `thread/resume`: same thread id, context kept; reply lists earlier turns with `clientId` | Start a new session; mark it "not resumed" | S1 `provider_thread_resume`, `matrix::resume_keeps_the_session_id_and_the_context`; S2 `provider_thread_resume`, `evidence::thread_resume_keeps_the_thread_and_its_context` |
| sessions | Fork | `--resume <id> --fork-session`: new session id, context kept | `thread/fork`: new thread id, context kept | Hide fork | S1 `fork_session`, `capabilities::fork_starts_a_new_session_with_the_old_context`; S2 `fork_thread`, `matrix::fork_gives_a_new_thread_with_the_old_context` |
| sessions | Open requests survive a provider restart | no: the resumed process has `pending_permission_requests: []` and does not ask again; a new prompt makes a new request with a new native id | no: the resumed thread does not ask again; native request ids restart at 0 | Request becomes `approval.expired { reason: not_resumable }` | S1 `approval_restart`, `replay_fixtures::open_approval_does_not_survive_a_restart`; S2 `approval_restart`, `evidence::an_open_approval_does_not_survive_a_restart` |
| turns | Interrupt | `control_request` `interrupt` (reply `still_queued`); `result` `error_during_execution`, also during a tool; a request before the turn starts cancels the input | `turn/interrupt` (reply `{}`), then `turn/completed` `interrupted`; needs the native turn id, so the adapter holds an early interrupt until `turn/started` | Stop the process (`StopMode::Kill`) | S1 `turn_interrupt`, `interrupt_during_tool`, `interrupt_right_after_send`, `replay_fixtures::interrupts_end_the_turn_as_interrupted`, `capabilities::interrupt_before_the_turn_started_cancels_the_input`; S2 `evidence::an_interrupt_during_a_tool_ends_both`, `evidence::an_interrupt_before_turn_started_is_held` |
| turns | Steer mid-turn | yes: a user message written during a turn joins the running turn at the next model step (after the running tool result) | yes: `turn/steer` with `expectedTurnId` joins the running turn (a mid-turn `turn/start` also joins it) | Interrupt + restart with the steer text as a "steering replacement attempt" | S1 `message_steering`, `replay_fixtures::steering_folds_into_the_running_turn`, `evidence::a_steer_prompt_starts_inside_the_running_turn_after_a_tool_result`; S2 `message_steering`, `evidence::a_steer_joins_the_running_turn` |
| turns | Follow-up queue | yes, native: a prompt written while idle-pending or busy is `command_lifecycle` `queued` and runs as its own turn | no (stable API): `turn/start` during a turn joins it (`thread/queue/*` is experimental); the adapter queues host-side (DL-015) | Station queues the input until `turn.completed` | S1 `queued_turn`, `replay_fixtures::queued_turn_runs_as_two_turns`, `evidence::a_queued_prompt_is_confirmed_when_its_turn_starts`; S2 `queued_turn`, `evidence::queued_prompts_run_as_separate_turns`, `adapter::tests::a_busy_prompt_waits_host_side_and_runs_as_its_own_turn` |
| turns | Terminal status quality | strong: one `result` per turn (`subtype`, `terminal_reason`) | strong: one `turn/completed` per turn (`completed` / `interrupted` / `failed`) | `strong` / `weak` / `none`; `weak` or `none` → status uses idle timeout | S1 `matrix::every_turn_ends_with_one_definite_terminal_status` (all Rust fixtures), `reference_fixtures::sdk_and_rust_recordings_end_turns_the_same_way`; S2 `matrix::every_turn_ends_with_one_definite_terminal_status` (all fixtures) |
| streaming | Live deltas | yes: `--include-partial-messages` `stream_event` text and thinking deltas | yes: `item/agentMessage/delta`, reasoning delta notifications | Show completed messages only | S1 `matrix::live_deltas_arrive_before_the_completed_message`; S2 `matrix::live_deltas_arrive_before_the_completed_message` |
| tools | File-change telemetry | Edit/Write/MultiEdit tool results with `file_path` (also files outside the workspace, for example plan files) | `item/fileChange/*` items (absolute paths) and `turn/diff/updated` (relative paths, aggregated diff) | Diff from the workspace service only | S1 `file_edit`, `capabilities::write_tool_results_map_to_file_changes`; S2 `file_change`, `evidence::diff_notifications_report_each_file_once` |
| approvals | Answerable approvals | yes: `control_request` `can_use_tool`; option ids `allow`, `allow_always` (only with `permission_suggestions`), `deny` | yes: command and file-change `requestApproval`; option ids = native `availableDecisions` (under `untrusted`: `accept`, `acceptWithExecpolicyAmendment`, `cancel`; file change adds `acceptForSession`, `decline`) | Provider runs under its own sandbox policy; board shows no approval controls | S1 `tool_call_approve`, `tool_call_deny`, `evidence::approval_options_map_to_native_behaviors`, `adapter::tests::approval_answer_is_idempotent_and_uses_native_ids`; S2 `tool_call_approve`, `tool_call_deny`, `evidence::approval_option_ids_are_native_decisions`, `evidence::file_change_approvals_use_native_decisions` |
| approvals | Response channel (L4) | `rpc_reply` for approvals and for questions (`AskUserQuestion` arrives as `can_use_tool`; answers go in `updatedInput.answers`) | `rpc_reply` for approvals and for `item/tool/requestUserInput` questions (experimental API); `message` for async questions in agent messages (feature `send_message_to_user_async`, not live in 0.160.0) | — | S1 `ask_user_question`, `capabilities::questions_are_answered_by_rpc_reply`; S2 `question_rpc`, `evidence::questions_use_the_rpc_reply_channel`, `adapter::tests::async_questions_are_answered_by_a_new_message`, `schema_methods::async_question_fields_exist_in_the_schema` |
| approvals | Edit input before allow | yes: `updatedInput` on allow; the CLI runs the edited input | no: the decisions carry no edited input; the adapter refuses an edit (`Unsupported`) | Allow / deny only | S1 `tool_call_edit_input`, `matrix::an_edited_input_runs_instead_of_the_requested_one`, `reference_fixtures::approval_answers_match_the_sdk`; S2 `adapter::tests::approvals_use_the_native_decisions_and_answers_are_idempotent` |
| approvals | Permission modes (native only) | `default` / `acceptEdits` / `plan` / `auto` / `dontAsk` / `bypassPermissions` (wire names; CLI `manual` = `default`, DL-003); `set_permission_mode` mid-session | `approvalPolicy` (`untrusted` / `on-request` / `never`) × `sandbox` (`read-only` / `workspace-write` / `danger-full-access`) on `thread/start` and per `turn/start` (DL-013) | Most restrictive mode | S1 `set_permission_mode`, `matrix::modes_are_native_and_change_on_request`, `adapter::tests::set_mode_accepts_only_native_modes`; S2 `set_mode`, `adapter::tests::modes_parse_and_fall_back_to_the_safest`, `adapter::tests::set_mode_applies_with_the_next_accepted_turn` |
| planning | Plan capture | `ExitPlanMode` tool request (`input.plan`, `planFilePath`) in `plan` mode | `plan` items with `collaborationMode` `plan` (experimental API) | No plan panel | S1 `plan_capture`, `capabilities::plan_mode_captures_the_plan_from_exit_plan_mode`; S2 `plan_capture`, `evidence::plan_mode_reports_the_plan` |
| subagents | Child work reported with parent link | yes: child frames carry `parent_tool_use_id` = the `Agent` tool call (the sub-agent runs async and can report after the turn) | yes: `collabAgentToolCall` items (`spawnAgent`, `wait`) name the child thread in `receiverThreadIds`; child-thread frames stream on the same connection | Child events attach to the parent run | S1 `subagent`, `capabilities::subagent_frames_link_to_the_parent_tool_call`; S2 `subagent`, `matrix::subagents_appear_as_tool_calls_of_the_parent_thread`, `adapter::tests_review::spawn_agent_links_its_child_thread_when_the_item_completes` |
| subagents | Pending background work after turn end | yes, pushed: `system/background_tasks_changed`, `task_started`/`task_notification`; a finished task starts a provider turn without host input | yes, polled: no notification; `thread/backgroundTerminals/list` shows a running terminal after `turn/completed`, and its `item/completed` arrives later | Session shows "background work", not "idle" | S1 `background_task`, `capabilities::background_work_reports_after_the_turn`; S2 `background_terminal`, `matrix::background_terminals_are_found_by_polling_after_the_turn` |
| identity | Native ids (session, turn, item) | `session_id`, `tool_use_id`, control `request_id`, input `uuid`; no native turn id | `threadId`, `turnId`, item `id`, `clientId`; JSON-RPC request ids | App allocates ids by ordinal (L6) | S1 `matrix::native_ids_are_kept_beside_ordinal_app_ids`; S2 `matrix::native_ids_are_kept_beside_ordinal_app_ids` |
| usage | Usage | `result` per turn: token `usage` and `total_cost_usd` | `thread/tokenUsage/updated` tokens; no cost (unknown); `usageLimitExceeded` error → `usage_limit` | Unknown ≠ zero | S1 `matrix::usage_comes_with_every_result`; S2 `matrix::usage_has_tokens_but_no_cost`, `adapter::tests::usage_limit_errors_get_their_own_code` |

Pi is out of v2 scope (D14). Research notes for the later Pi package: `pi --mode rpc` has native `steer`, `follow_up`, `abort`, `fork`, and `get_session_stats`; it has no permission system, and T3 Code gates tools with an injected extension (`pi.on("tool_call")` + `ctx.ui.confirm`), see [t3code-lessons.md](./t3code-lessons.md) L9.

### 6.3 Canonical events

All events are provider-blind. Each event has `session_id`, `session_seq`, `station_seq`, `at`, and `proto_version`.

**Ids (L6, L7).** App ids are primary: `session_id`, `run_id` (one user request, which can contain several native turns), `turn_id` (one provider turn), `node_id` (one tool call, approval, or subagent inside a run; `parent_node_id` links a child to its parent), and `request_id` (one approval or question). Native ids go in a `provider_refs` object on the event and are scoped by provider + session; they are never used as global keys. A provider with weak or no ids gets app-allocated ids by ordinal.

**Run invariants (L7).** A child (subagent) completion never closes the parent run. Background work can report after the parent turn ended; a session with pending background work is not idle.

```
session.started      { provider, provider_version, model, cwd, mode, provider_refs }
session.state        { state: starting|running|stopping|exited|lost, detail }
run.started          { run_id, input_summary, origin: board|mail|coordinator|resume }
turn.started         { run_id, turn_id, provider_refs }
message.delta        { turn_id, text }
message.completed    { turn_id, text }
reasoning.delta      { turn_id, text }                       (optional)
tool.started         { node_id, parent_node_id?, kind: shell|edit|read|search|web|mcp|subagent|other, title, input_summary, files[] }
tool.output          { node_id, chunk }                      (bounded, truncated)
tool.completed       { node_id, status: ok|failed|denied, files[], exit_code? }
file.changed         { path, change: added|modified|deleted }
approval.requested   { request_id, node_id?, kind: command|file_change|tool|question, details, options[{option_id, label}], response: rpc_reply|message }
approval.resolved    { request_id, option_id, by: board|policy|timeout|coordinator|local }
approval.expired     { request_id, reason: not_resumable|process_lost }
plan.proposed        { plan_id, markdown }
usage.updated        { input_tokens, output_tokens, cost_usd?, rate_limits? }   (unknown ≠ zero)
turn.completed       { turn_id, status: done|interrupted|failed, usage? }
run.completed        { run_id, status: done|interrupted|failed, background_pending }
error                { code, message }
session.ended        { reason, exit_code? }
```

### 6.4 Event log, command receipts, and effects (L1, L2)

- **One transaction per command decision.** The station validates a command, then commits in one SQLite transaction: the new events, the projection updates, the command receipt, and the outbox effects (for example "write this prompt to the provider"). The response to the deck is "accepted, committed at `station_seq` N". Subscribers see events only after the commit.
- **Effect worker.** A worker reads committed effects and does the provider and filesystem I/O outside the transaction. It reports results as new events. No external I/O happens inside a command decision or a transaction.
- **Effect states and at-most-once dispatch.** An effect is `queued → claimed → confirmed`, or ends as `retired_not_dispatched` or `outcome_unknown`. The worker persists the claim (with its worker fence token and the target provider incarnation) **before** it does the I/O. A claimed effect is never sent again automatically, also after a worker crash: the station accepts possible non-delivery instead of duplicate agent work. This is at-most-once *station dispatch*, not a guarantee about downstream tool execution. Each adapter documents what "confirmed" means (a local pipe write is not provider acceptance; acceptance is not turn completion).
- **Recovery.** On start, in one transaction and before it admits new commands, the station retires `queued` effects that target a dead provider incarnation as `retired_not_dispatched`, marks `claimed` effects without confirmation as `outcome_unknown`, expires runtime requests whose response channel is gone, and writes the lifecycle events and projections. It reconciles unknown outcomes from provider evidence (for example a resumed thread history) where available, without a resend. The owner can submit a new command. It never replays a prompt into a new process automatically.
- **Receipts acknowledge acceptance, not completion.** A duplicate command returns the original accept/reject receipt; later events describe delivery and execution. A reused `command_id` with a different method, payload, or caller identity is rejected. Receipts are kept longer than any retry window of the deck.
- `station_seq` is one monotonic counter per station, allocated in the same SQLite transaction that stores the event. The deck uses it as the resync cursor.
- `session_seq` is a per-session counter for ordering in the UI.
- The station keeps events until the deck acknowledges them **and** the retention window passes. If a deck cursor is older than the retained history, the station sends a session snapshot instead of a replay.
- The station has an `incarnation_id` (a new value when its database is reset). A deck that sees a new incarnation drops its cursor and resyncs from a snapshot.
- Command receipts: `command_id → {state: accepted|rejected, station_seq, result, at}`. A duplicate `command_id` returns the stored receipt and does nothing else.
- Runtime requests (approvals, questions) are persisted records with the provider's own option ids. Answering a request is idempotent: a repeated answer returns its receipt (L3).

### 6.5 Approvals when the deck is not reachable

Each session has an `offline_approval_policy`:

| Policy | Behaviour |
| --- | --- |
| `wait` (default) | The approval stays open until the deck reconnects and the owner answers |
| `deny_after(duration)` | The station denies the request after the duration and records `by: timeout` |

The permission mode decides which approvals occur at all. After a station restart (or a provider process loss), an open approval is shown as expired (`approval.expired`), not as answerable, unless the provider capability "open requests survive a provider restart" is true (L3). An emergency local path also exists: `fleetdeck station sessions` and `fleetdeck station approve <id>` work on the station machine without the deck. This is the v2 version of v0's "fail-open floor".

### 6.6 Workspace service

- Repo catalog per station (path, origin URL scrubbed, default branch).
- Worktrees: `<repo>--fd-<callsign>` on branch `fd/<callsign>` (or ticket-first, as in v0).
- Clone on the station with the station's own `git` credentials. The deck never holds forge tokens.
- File browse, search, read; diffs against the recorded base ref; per-turn checkpoints (shadow refs at `turn.started` and `turn.completed`) with revert.
- Repo grouping for the conflict radar: a normalized origin URL is a **heuristic**, not an identity. Repos with several remotes or no remote are grouped only by explicit owner choice.

### 6.7 Station policy (local file, owner-edited)

```toml
# ~/.config/fleetdeck/station.toml
name = "laptop"
allowed_roots = ["~/projects", "/workspace"]
max_sessions = 12
allow_setup_cmd = false
allow_shell = true            # PTY shells from the board (D10)
max_shells = 8
offline_approval_policy = "wait"
listen = []                   # deck-dials-station mode only; loopback, LAN, or tailnet addresses

# Provider instances (T3 lesson L10). Work is routed by instance id, never by driver.
# Two accounts on one driver are two instances and never share session state.
# The station removes ambient provider credentials that the instance does not configure.
[[provider_instance]]
id = "claude"                  # stable id shown in the board and stored on each session
driver = "claude"              # claude | codex
binary = "claude"              # optional path; default: driver name on PATH
config_home = ""               # optional: CLAUDE_CONFIG_DIR / CODEX_HOME for this instance
env = {}                       # optional extra environment for this instance

[[provider_instance]]
id = "codex"
driver = "codex"
```

### 6.8 Shell service (D10)

The owner can open a plain shell in a repo or worktree from the board. It is for the human, not for agents, so it does not conflict with "drive only" (D3).

- The station owns the PTY (`portable-pty`). It runs the owner's login shell as the station's OS user, with the scrubbed environment (§3 rule 6).
- The cwd must be inside `allowed_roots`. `allow_shell = false` disables the service.
- Bytes go over the link **data** channel, never the control channel (A7).
- The station keeps a scrollback ring buffer per shell: at most 5,000 lines and 8 MiB (the T3 Code limits). A board that reconnects receives the buffer, then live bytes.
- A shell continues when the deck, the board, or the network fails (doctrine rule 2). A station restart ends it in the MVP (A10).
- Open, resize, and close are audited commands with a `command_id`. Keystrokes are not audited.

> **Caution:** `allowed_roots` is a guardrail, not a sandbox. An agent with shell access can read and write anything that the station's OS user can. A paired deck can effectively run code as that user. On a shared or sensitive machine, run the station as a dedicated OS user or in a container.

---

## 7. Deck design

### 7.1 Services

| Service | Job |
| --- | --- |
| Link manager | Accepts inbound station links, dials configured stations, keeps one active link per station |
| Station registry | Paired stations, keys, names, roles, last seen, versions, providers, capacity |
| Read model | Applies station events, derives card status, keeps per-session timelines for the board |
| Command router | Sends board/coordinator commands to stations with `command_id`; tracks pending and ambiguous commands |
| Outbox | Durable deck→station intents (mail delivery, task assignment) with retries and receipts |
| Fleet services | Callsigns, routing (`assign auto` across stations), mail, plans, tasks/completions, conflict radar, usage meters |
| Board API | REST for commands; WS for snapshot + deltas |
| Coordinator API | Scoped tokens for agent sessions that drive the fleet (read, mail, drive, spawn classes; spawn caps) |
| Auth and audit | Owner login, sessions, CSRF, audit log of every mutation |

### 7.2 Status projection

Columns: `queued → working → verifying → needs-you → idle → ended`, plus two link states that v0 did not need:

- **unreachable** — the station link is down. The card shows the last known state with a badge. The session can still be running.
- **lost** — the station restarted and the process is gone. The card offers "resume".

`verifying` means a running shell tool matches the test-runner pattern (as in v0). `needs-you` means an open approval or question.

### 7.3 Routing

`assign auto: <task>` picks a target with a SQL query (as in v0: idle first, least buried, right repo) and adds station terms: the station is connected, the repo exists there, the station is under `max_sessions`, and the provider is available. If no session fits, the board offers "spawn on <best station>".

---

## 8. Link protocol

> **Revision 2 (M1 gate, 2026-10-11).** GATE-M1 G1–G7 changed this section. ADR-004 §9 has the full contract; DL-040 logs the change.

### 8.1 Transport

- One **control** WebSocket and one **data** WebSocket per link, both over TLS (rustls), both opened by the dialing side.
- After the handshake, the direction does not matter. Both sides can send requests.
- Control frames: ≤ 1 MiB, JSON. Data channel: multiplexed byte streams, chunks ≤ 64 KiB, credit-based flow control per stream.
- Reconnect: exponential backoff with jitter, by the dialing side.
- Budgets per link (G2): at most 64 open streams, a 256 KiB window per stream, 4 MiB of buffered inbound bytes for the connection (counted until the application consumes them), control queues of 256 messages each, and 128 outstanding requests. An empty data frame is a protocol error. A violation closes the link with `flow_control_violation`. A data stream opens only with a single-use stream token from the control response that authorized it (G6).
- Network scope (D13): a node listens and dials only on loopback, private LAN ranges (RFC 1918, IPv6 ULA, link-local), and the Tailscale ranges (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`). Any other address is rejected at bind and at dial time. Tailscale identity is not used as authentication; mutual TLS with pinned keys stays required.

### 8.2 Handshake

1. TLS with self-signed certificates derived from each node's Ed25519 identity key. Both sides pin the peer's key after pairing (mutual TLS).
2. `Hello { proto_min, proto_max, features[], role, node_id, link_epoch, nonce }` in both directions. Feature flags, not only the version, decide behaviour; old fields stay for old peers (L16).
3. Each side signs the handshake transcript (both identities, both roles, both nonces, the TLS exporter value). This binds the link to this TLS session.
4. The deck checks that the peer key belongs to a paired station with role `station`. The station checks that the peer key is its paired deck.
5. Protocol version = the highest common version. No common version → the link closes with a clear error on both sides.
6. One node-wide link table, keyed by the peer key fingerprint, covers inbound and outbound links and all listeners (G1). `link_epoch` is a persistent per-peer counter of the dialer, committed before each dial; each attempt also has a random 128-bit `attempt_id`, and the data channel binds to it. If a second link to the same peer reaches Ready: from the same dialer, the higher epoch wins; from opposite dialers, the link dialed by the node with the lower key fingerprint wins. The losing link closes with `superseded`, and its frames are dropped. A stale epoch is refused with `stale_link_epoch`.
7. Every frame is checked against the roles bound at pairing, never against the dial direction (G6).

### 8.3 Frames (control channel)

```
Request          { id, command_id?, method, params }
Response         { id, ok | err }
Event            { incarnation, station_seq, session_id, session_seq, event }   // station → deck
Ack              { incarnation, station_seq }            // deck → station, after the read model commits
SnapshotBegin    { incarnation, watermark, session_count }   // station → deck, when replay is not possible
SnapshotSession  { incarnation, snapshot }
SnapshotEnd      { incarnation, watermark }
Ping/Pong
Close            { reason, detail }
```

- A snapshot is a batch (G4). The deck applies it and sets its cursor to `watermark` in one transaction at `SnapshotEnd`; sessions that are not in the batch are marked `removed`; a batch without its end has no effect. Snapshot frames need the `snapshots.v1` feature.
- An `Ack` with another incarnation, or with a sequence above the delivered watermark, is a protocol error. A frame in the wrong direction is a protocol error (G6).

### 8.4 Command semantics

- Every link method and every board-API method declares its required scope. An authenticated socket does not authorize every method (L15).

- The deck gives every mutating command a ULID `command_id` and stores it as `pending` before it sends it.
- If the link drops before the response, the deck re-sends **the same** `command_id` after reconnect. The station returns the stored accept/reject receipt or decides the command once; delivery and execution outcomes arrive later as events.
- Every mutating command also carries `target_incarnation` (the station incarnation the deck last saw) and `retry_until` (deck UTC time; default accepted-at + 15 min). The deck stores the envelope before it sends it, and the stored envelope never changes (G3).
- The station rejects a command for another incarnation (`incarnation_mismatch`) or with a `retry_until` more than 2 min in the past (`expired`), and stores no receipt for these two rejections. Stations keep receipts for at least 24 h.
- The deck never resends after `retry_until`. A command that was sent and has no receipt by then, or that was sent before and is now rejected with `incarnation_mismatch` or `expired`, becomes `ambiguous`. Such a rejection of a command that was never sent makes it `failed` (`station_reset`). The board shows an ambiguous command and asks the owner. Nothing is redispatched automatically, and nothing is retried with a new id.
- Coordinator operations that change fleet intent (`coordinator.call`) are mutating: they need a `command_id`, and the deck keeps receipts and deduplicates them like station receipts. Read operations (`coordinator.query`) are not mutating (G7). ADR-004 §9.6 has the method table with the caller and receiver role, scope, feature and mutating flag of every method.

### 8.5 Pairing

**Station dials deck:**

1. The owner runs `fleetdeck deck invite` (or clicks "Add station" on the board).
2. The deck makes an invitation: `fdinv1:<deck-url>#<deck-key-fingerprint>.<secret-128-bit>.<expiry>`. It is single-use and expires after 15 minutes.
3. The owner runs `fleetdeck station join <invitation>` on the station.
4. The station connects, checks the deck's key against the fingerprint in the invitation, and proves the secret bound to the TLS transcript.
5. The deck consumes the invitation, pins the station key with role `station`, and assigns a name.

**Deck dials station:** the same flow in the mirror direction. `fleetdeck station invite --listen <addr>` makes the invitation; the owner pastes it into the board.

**Rules:**

- Only the authenticated owner can add dial targets. The deck does not follow redirects. LAN discovery (mDNS) only suggests candidates.
- Revoke: "remove station" on the deck unpins the key; `fleetdeck station forget-deck` on the station does the same on its side. A revocation removes the pin and all grants of the key in one transaction and closes every active link of that key within 1 s. The decision transaction of each command checks that the pin and the grant still exist, so a queued command from a revoked link is rejected (G5).
- Key rotation: a paired node signs a new key with its old key; the peer replaces the key in one transaction. The new key gets only the role and grants of the old key, and the old key is revoked as above (G5).
- Invitation input (G11, G12): `fleetdeck station join` and `fleetdeck deck add-station` read the invitation from a non-echoing prompt, stdin, or `--invite-file` (mode 0600), never from an argument. Five wrong proofs cancel only that invitation; each source IP gets at most 10 pairing attempts per minute, and at most 4 pairing handshakes run at the same time.

---

## 9. Security model

**Trust statement.** The deck is the root of execution trust. A paired deck can run code on every station as the station's OS user. The design limits who can become or control the deck; it does not try to contain a compromised deck. Station policy is a guardrail. Real containment requires a dedicated OS user or a container.

| Threat | Control |
| --- | --- |
| Someone reaches the board | Login required (also on loopback, A13); argon2 password; passkeys later; `HttpOnly`, `Secure`, `SameSite=Strict` cookie |
| Over-broad authorization | One required scope per board-API method and per link method; a socket login does not authorize every method; bearer clients get short-lived WS tickets, so long-lived tokens stay out of URLs (L15) |
| CSRF / cross-site WS | CSRF token on every mutating request; WS `Origin` check against the deck's own listen addresses and configured tailnet hostname |
| Exposure to the internet | D13: the deck and station refuse public addresses at bind and dial time (§8.1) |
| Shell misuse | Shells are owner-only, `allow_shell` per station, cwd inside `allowed_roots`, open/close audited (§6.8) |
| Rogue station or rogue deck | Mutual TLS with pinned keys; signed handshake transcript; roles bound at pairing; every frame and method checked against those roles and a fixed direction (G6) |
| Revoked key with an open link | Revocation closes the key's links within 1 s; each command's decision transaction re-checks the pin and grant (G5) |
| Duplicate or reciprocal links | One node-wide link table; persistent per-peer dialer epochs and `attempt_id`; a deterministic winner; frames of a superseded attempt are dropped (G1) |
| Memory exhaustion over the link | Per-link stream, window, buffer, queue, and request budgets; violation closes the link (G2) |
| Invitation theft | 128-bit secret, single use, 15-minute expiry, bound to the deck's key fingerprint; the secret stays in the URL fragment (after `#`), never in a query; the board strips it from history; pairing secrets never appear in a read model (L15); never in argv, logs, or events; failures counted per invitation and rate-limited per source IP (G11, G12) |
| SSRF through deck dial targets | Only the owner configures targets; only loopback, LAN, and tailnet addresses; no redirects; discovery is not trust |
| Prompt injection from forge text or agent output | Third-party text is fenced and labeled as untrusted data; issue-driven spawns default to supervised |
| XSS from agent output | Markdown rendered without raw HTML; strict CSP; no `dangerouslySetInnerHTML` on agent text |
| Credential leak | Provider credentials stay on the station; `Secret<T>` cannot be serialized; env scrub for children; no secrets in argv or logs |
| Coordinator agent overreach | Scoped token classes and spawn caps; token passed by env, never argv; every use audited |
| Replay of old commands | Command ids + receipts; `target_incarnation` and `retry_until` on every mutating command, so a reset station never runs a resent command (G3); link-bound signatures |

The adversary agent does a DESIGN REVIEW of this section and of §8 before Phase 3 freezes the link protocol.

---

## 10. Data model (first sketch)

**station.db**

| Table | Key columns |
| --- | --- |
| `meta` | `incarnation_id`, `station_seq_next`, identity key reference |
| `pairing` | `deck_key`, `deck_url?`, `paired_at` |
| `links` | link state (G1, G5): `link_epochs` (`peer_key`, `epoch`: the dialer's per-peer counter and the listener's highest accepted epoch), `grants` (`peer_key`, `scope`), `revocation_generation` |
| `sessions` | `session_id`, `provider`, `native_session_id`, `cwd`, `worktree`, `mode`, `state`, `offline_approval_policy`, `created_at` |
| `events` | `station_seq` PK, `session_id`, `session_seq`, `kind`, `payload` JSON (incl. `provider_refs`), `raw_ref?`, `at` |
| `effects` | outbox of the effect worker: `effect_id`, `command_id`, `session_id`, `process_incarnation`, `kind`, `payload`, `state: queued|claimed|confirmed|retired_not_dispatched|outcome_unknown`, `fence_token?`, `reason?` |
| `runtime_requests` | `request_id`, `session_id`, `node_id?`, `kind`, `options` JSON (native option ids), `response_channel`, `state: open|resolved|expired`, `receipt?` |

Raw provider frames are not a table: bounded rotating log files per session (L12).
| `shells` | `shell_id`, `cwd`, `state`, `created_at` (scrollback stays in memory) |
| `commands` | receipts: `command_id` PK, `method`, `state: accepted|rejected`, `station_seq`, `result` JSON, `at` |
| `repos`, `worktrees`, `checkpoints` | workspace data |

**deck.db**

| Table | Key columns |
| --- | --- |
| `owner`, `login_sessions` | auth |
| `stations` | `station_id`, `name`, `key`, `role`, `dial_url?`, `direction`, `last_seen`, `incarnation_id`, `cursor` |
| `sessions` | read model: `session_id`, `station_id`, `callsign`, `provider`, `status`, `model`, `repo_group`, ... |
| `timeline` | condensed per-session timeline for the board |
| `commands` | `command_id`, `station_id`, `state: pending|done|failed|ambiguous`, `origin`, the stored envelope (`target_incarnation`, `retry_until`, G3), `sent_at?` |
| `links` | `link_epochs`, `grants`, `revocation_generation` as in station.db (G1, G5) |
| `coordinator_receipts` | receipts of mutating `coordinator.call` commands, same rules as station receipts (G7) |
| `outbox` | `command_id`, `station_id`, `kind`, `payload`, `attempts`, `next_at`, `expires_at` |
| `mail`, `plans`, `tasks`, `completions` | fleet intent |
| `file_touches` | conflict radar input |
| `tokens` | coordinator token classes and caps |
| `audit` | every mutation: who, what, when, result |
| `settings` | deck settings |

---

## 11. Board (web client)

React + TypeScript + Vite in `web/`. Types come from `web/src/gen/` (generated by `ts-rs`). CI fails if the generated files are out of date. We port design tokens, layout ideas, and proven components from `board/src` where they still apply.

| Surface | v2 content |
| --- | --- |
| Fleet board | Status lanes; each card shows callsign, station badge, provider badge, model, repo/branch, current tool, usage |
| Stations panel | Health, link direction, version, providers + versions, capacity, last seen; "Add station" (pairing) |
| Session view | Turn timeline (messages, tool calls, diffs, approvals inline); composer with prompt / steer / follow-up; interrupt; mode ladder (only the modes in the session's capabilities, L8); model picker |
| Session grid | Many live session views at once (replaces the v0 terminal grid) |
| Shell | xterm.js terminal for a station shell in a repo or worktree (D10); can sit in the session grid |
| Needs-you rail | Approvals from the whole fleet; allow once / allow always / deny / edit input where the provider supports it |
| Spawn form | Station (or "auto"), provider, directory picker on the station, branch/worktree, batch + `Nx`, permission mode, offline policy |
| Compose / mail | To a callsign, a repo group, a station, or everyone |
| Plans, worktrees, files, diffs | As v0, routed through the deck to the owning station |
| Settings, audit log | New: audit log viewer |
| Mobile layout | Approvals and interrupts are first-class on a phone |

**Connection runtime (T3 lesson L17, Phase 4).** `web/src/runtime/` owns the deck connection:

- One retry owner for the deck connection (jittered backoff, cap 30 s in the browser).
- Transport health and data freshness are separate states. "Connected, timeline subscription failed" is not "reconnecting".
- Cached data stays readable while offline, is marked stale, and never overwrites newer live data.
- Session timelines use per-session subscriptions with a cursor. The runtime keeps an idle subscription cursor for 5 minutes, so back navigation does not load a snapshot again. The fleet board uses one summary stream.

---

## 12. Repository layout and stack

The `v2` branch adds new directories. v0 code stays untouched for reference until the cutover in Phase 7, which deletes it.

```
Cargo.toml                    # workspace
crates/
  fleetdeck-proto/            # wire types, canonical events, link frames, versions (ts-rs export)
  fleetdeck-core/             # ids, config, identity keys, Secret<T>, SQLite + migrations, logging
  fleetdeck-link/             # TLS, handshake, pairing, control + data channels, reconnect
  fleetdeck-providers/        # adapter traits + claude/, codex/ (pi/ and acp/ later)
  fleetdeck-station/          # session manager, SessionHost, event log, receipts, effect worker, workspace, shell, policy
  fleetdeck-deck/             # registry, read model, fleet services, outbox, HTTP/WS API, embedded board
  fleetdeck-cli/              # bin `fleetdeck`
  fleetdeck-testkit/          # fake-claude, fake-codex, recorders, harness, fault injection
web/                          # React + TS board
docs/v2/                      # this plan, ADRs, internals glossary
```

| Concern | Choice |
| --- | --- |
| Async runtime | `tokio` |
| HTTP / WS | `axum`, `tokio-tungstenite` |
| TLS / certs | `rustls`, `rcgen`, `ed25519-dalek` |
| Serialization | `serde`, `serde_json` (debuggable wire; a binary encoding only if profiling asks for it) |
| SQLite | `rusqlite` (bundled, WAL) on a dedicated DB thread; `rusqlite_migration` |
| TS types | `ts-rs` |
| CLI | `clap` |
| Logs | `tracing`, `tracing-subscriber` (JSON option) |
| Board assets | `rust-embed` |
| LAN discovery | `mdns-sd` |
| PTY | `portable-pty` (station), `xterm.js` (board) |
| Password hashing | `argon2` |
| IDs | `ulid` |
| Errors | `thiserror` in libraries, `anyhow` in the binary |
| Tests | `cargo-nextest`, `insta`, `proptest` for protocol parsers |
| Supply chain | `cargo-deny` |
| Release | `cargo-dist` (shell installer, Homebrew, GitHub releases) for Linux and macOS, plus a station container image |
| Rust | stable, edition 2024, pinned MSRV |

---

## 13. Test strategy

| Layer | How |
| --- | --- |
| Protocol parsers | Unit tests + `proptest` fuzzing of every provider frame parser; unknown frames are logged and ignored; unknown permission requests **fail closed** |
| Adapters (L20) | **Replay-fixture testing first.** A `record-<provider>` tool per adapter records real transcripts. Replays run through the real adapter, event store, projections, and checkpoint policy; only the provider transport, clock, id source, temp filesystem, and database are substituted (`Clock` and id sources are injected, so replays are deterministic). Shared scenarios for every provider: `simple`, `multi_turn`, `queued_turn`, `turn_interrupt`, `message_steering`, `provider_thread_resume`, `tool_call_approve`, `tool_call_deny`. Scripted fakes cover only cases that a recording cannot produce (crashes, malformed frames). Snapshots with `insta` |
| Live smoke | Nightly job against pinned and latest CLI versions of Claude and Codex. A new CLI release is supported only after it passes |
| Link | Both directions, pairing, reconnect, version mismatch, feature-flag mismatch, duplicate links, NAT case (a station with no listener that only dials), multi-process on loopback with separate data directories; bind and dial rejection of public addresses |
| Shell | PTY open, resize, scrollback cap, reattach after a board reconnect, survival of a link drop |
| Fault injection | Crash at each effect boundary: before commit, after commit, after the dispatch claim, after the send, before the confirmation is stored; worker restart and provider restart as separate cases (incl. thread resume and a stale approval reply); crash between execute and acknowledge; disconnect during an open approval; duplicate replays; simultaneous links; slow board consumer; station database loss (new incarnation); cursor older than retention |
| End to end | `fleetdeck up` + 2 extra stations in containers; Playwright tests for the board's main flows |
| Performance bars | Approval round trip board→agent < 300 ms on LAN; deck read-model lag p95 < 500 ms; 50 sessions per station; 10 stations per deck; station overhead < 30 MB + < 5 MB per session (agent process excluded) |

---

## 14. Phases

Each phase ends with an exit check. Phases 2–4 are one **vertical slice**: auth, deduplication, and crash recovery are in the slice, not deferred to hardening.

Work method (from `AGENTS.md`): the architect splits each phase into task briefs; workers run in their own worktrees branched from `v2`; the code reviewer reviews each branch before the merge into `v2`; the adversary reviews every contract boundary.

### Phase 0 — Spikes (de-risk, ~1–2 weeks)

| Spike | Must prove | Output |
| --- | --- | --- |
| S1 Claude from Rust | spawn, initialize, prompt, partial deltas, `can_use_tool` allow/deny with edited input, interrupt during a tool, resume, set permission mode, unknown-frame handling. Record the TypeScript SDK's frames for the same scenarios and replay them against the Rust adapter. Cover the shared replay scenarios (§13), the response channel, and whether open requests survive a restart | ADR-001: native Claude adapter design + recorded fixture set (A8, D8) |
| S2 Codex app-server from Rust | initialize, `thread/start`, `turn/start`, both approval requests, the response channel of questions (L4: RPC reply or new user message), `turn/interrupt`, `thread/resume`, diff notifications. Record the shared replay scenarios (§13) | ADR-002: depend on `codex-app-server-protocol` (git pin) **or** codegen from `codex app-server generate-json-schema` |
| S4 Link | both directions, mTLS with pinned keys, invitation pairing, control + data channels, reconnect, feature-flag handshake, NAT case (station only dials), tailnet address rules (unit tests; a real tailnet check is an owner step) | ADR-004: link protocol v1 (adversary DESIGN REVIEW) |

(S3, the Pi spike, is removed by D14. The numbers stay stable.)

**Exit:** three ADRs approved by the owner. The capability matrix (§6.2) is filled with verified values and evidence for every cell.

### Phase 1 — Foundations

- Cargo workspace, all crates as skeletons, `fleetdeck` binary with subcommands.
- CI: `cargo fmt --check`, `clippy -D warnings`, `nextest`, `cargo-deny`, TS-types drift check, `web` typecheck + lint + test.
- `fleetdeck-proto` v1: canonical events, link frames, commands, version negotiation.
- `fleetdeck-core`: identity keys, config, `Secret<T>`, SQLite + migrations, logging.
- `fleetdeck-testkit` skeleton; `web/` skeleton with ported design tokens.

**Exit:** CI green on an empty-but-wired workspace; generated TS types imported by `web/`.

### Phase 2 — Station core (slice, part 1)

- `SessionHost` trait + `DirectChildHost`; process groups; clean stop and kill.
- Claude adapter (per ADR-001); fake-claude.
- Event log with `station_seq`, incarnation, retention; command receipts + effect worker (one transaction, A4); startup retirement of dead effects (A10); policy file.
- Station-local CLI (`fleetdeck station sessions|run|approve`) over a Unix socket.

**Exit:** a Claude session runs, asks for approval, and is answered and interrupted from the local CLI; fault tests for crash-before-ack pass.

### Phase 3 — Deck core + link (slice, part 2)

- Link (per ADR-004) in both directions; pairing; registry; duplicate-link rule.
- Read model sync: cursor, ack, snapshot fallback, incarnation change.
- Command router with pending/ambiguous states; outbox.
- Auth: owner login, sessions, CSRF, WS Origin check; audit log.
- Board API: REST + WS snapshot/deltas; status projection.

**Exit:** a deck with 2 stations (one dials in through NAT, one is dialed); link drops and deck restarts lose no events and duplicate no commands.

### Phase 4 — Board MVP (slice, part 3)

- Login, stations panel, add station (pairing), fleet lanes, session view (timeline, composer, interrupt, mode), needs-you rail, spawn form with station + provider + directory picker.
- `fleetdeck up` all-in-one mode.

**Exit (MVP demo):** from a phone, the owner spawns Claude sessions on two different machines, answers approvals, steers and interrupts them. The demo survives a deck restart and a network drop.

### Phase 5 — Codex

- Codex adapter (per ADR-002); fake-codex and recorded fixtures.
- Capability-driven UI: controls appear only when the session's capabilities allow them.

**Exit:** the same board flows work for Claude and Codex; tests cover the capability matrix.

### Phase 6 — Fleet features (parity and beyond)

- Worktrees, batch spawn, `Nx`, repo catalog, clone on the station.
- Mail through the outbox, delivered as native follow-up/steer.
- `assign auto` across stations.
- Tasks and waitable completions; coordinator API with token classes and spawn caps; a deck-served control skill for coordinator agents.
- Plans (capture from Claude `ExitPlanMode` and Codex plan items), plan library.
- Conflict radar across stations.
- Files, diffs, per-turn checkpoints with revert.
- Usage and rate-limit meters ("unknown" is never shown as zero).
- Screenshot/image paste into prompts; notifications.
- Human shell (§6.8, D10).
- Import an existing CLI session as a driven session.

**Exit:** every "Keep" row in §4.1 and every row of the parity table (§16) is done or explicitly deferred by the owner.

### Phase 7 — Hardening and cutover

- `ShimHost`: agents survive a station restart or upgrade; the station reattaches.
- Station drain and self-upgrade; deck and station version-skew rules (N−1 compatibility).
- Security: adversary PRE-PR REVIEW of the whole branch; fix all findings.
- Performance bars (§13) met and recorded.
- Packaging: `cargo-dist`, systemd and launchd units, station container image.
- Docs: new README, `docs/v2/internals.md` (glossary, crate map, protocol), operations guide.
- Cutover on the `v2` branch: delete v0 code (`src/`, `board/`, `hooks/`, `scripts/`, plugin files), merge `v2` into `main`, release `2.0.0`.

**Exit:** adversary approval of the final HEAD; owner sign-off; merge.

---

## 15. Owner answers (2026-10-11)

| # | Question | Answer | Recorded as |
| --- | --- | --- | --- |
| 1 | Claude adapter: native Rust or SDK sidecar | Rust adapter only; learn from the TypeScript SDK; no TypeScript sidecar | D8, A8, S1 |
| 2 | MVP durability | Yes | D9, A10 |
| 3 | Human shell | Yes, it is useful | D10, §6.8, Phase 6 |
| 4 | v0 data import | No | D11 |
| 5 | Windows stations | Linux and macOS only | D12, A11 |
| 6 | Remote access to the deck | Local and tailnet only | D13, doctrine rule 5, §8.1, §9 |
| 7 | Pi approvals | Pi is out for now; later, possibly as a full Pi package | D14, Phase 5 |

No owner questions are open. The architect's next step is to apply the P1 rows of [t3code-lessons.md](./t3code-lessons.md) (with an advisor review of L1 and L14 first).

---

## 16. Parity table: v0 → v2

| v0 feature | v2 | Phase |
| --- | --- | --- |
| Live derived status lanes | Keep (from canonical events) + `unreachable` / `lost` | 3–4 |
| Model badge follows `/model` | Keep (from session and model events) | 4 |
| Conflict radar | Keep, across stations | 6 |
| Needs-you rail | **Better:** approvals answered from the board | 4 |
| Re-armed questions | Not needed: the protocol holds the request open; the offline policy covers timeouts | — |
| Mail between sessions | Keep, native delivery through the outbox | 6 |
| `assign auto` routing | Keep, plus station terms | 6 |
| Spawn, batch, `Nx` | Keep, plus a station picker | 4 / 6 |
| Live terminal (xterm.js on tmux) | Replaced by the session view; a separate human shell (D10) | 4 / 6 |
| Terminal grid | Replaced by the session grid | 4 |
| Shift+Enter, Ctrl+V screenshot paste | Composer newline; image paste into prompts | 4 / 6 |
| Revive | Native resume | 4 |
| Remote control (claude.ai) | Dropped; the deck is the remote | — |
| Plan library | Keep; capture from the protocol | 6 |
| Workspace tools (files, worktrees, rename, event feed) | Keep, per station | 6 |
| Share board on LAN | Replaced by deck access on the LAN and tailnet + station discovery | 3 / 7 |
| Adopt / takeover a terminal session | Replaced by "import session id" | 6 |
| Gateway routing (LLM gateway env) | Keep as station provider profiles | 6 |
| Coder support | Dropped (D13: local network and tailnet only) | — |
| `/fleet` slash command, fleet-doctrine skill | Replaced by a deck-served control skill for coordinator agents | 6 |
| Plugin install + version-bump channel | Replaced by a binary release | 7 |
