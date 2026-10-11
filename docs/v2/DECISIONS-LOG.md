# Fleet Deck v2 — decisions log

This log records each deviation from PLAN.md and each choice that the plan does not state. The goal loop runs without owner access, so each entry has "owner review: pending" until the owner accepts or changes it.

Format: context, options, choice, reason, owner review.

---

## DL-001 — Cargo workspace in Phase 0 holds only the spike crates

- **Context:** PLAN §12 places the workspace `Cargo.toml` at the repository root with the product crates under `crates/`. Phase 1 creates those crates. The M1 checks (`cargo fmt --all --check`, `cargo clippy --workspace`, `cargo nextest run --workspace`, `cargo deny check`) must run on the spike crates in Phase 0.
- **Options:** (a) a separate workspace in `spikes/Cargo.toml`; (b) the root workspace now, with members `spikes/*` only.
- **Choice:** (b). The root `Cargo.toml` lists only `spikes/*` members in M1. Phase 1 adds `crates/*` and decides if the spikes stay members or move to `exclude`.
- **Reason:** the checks in the goal run from the repository root. One workspace gives one `Cargo.lock`, one `deny.toml`, and one lint policy that Phase 1 reuses.
- **Owner review:** pending

## DL-002 — MSRV pin without a `rust-toolchain.toml`

- **Context:** PLAN §12 asks for "stable, edition 2024, pinned MSRV". The installed stable toolchain is 1.96.0. A `rust-toolchain.toml` with `1.96` makes rustup install a second named toolchain (network access, a slow first build).
- **Options:** (a) `rust-toolchain.toml` with `channel = "1.96"`; (b) `rust-version = "1.96"` in `[workspace.package]` only.
- **Choice:** (b) for M1. Phase 1 CI can add the toolchain file when CI installs toolchains anyway.
- **Reason:** `rust-version` pins the MSRV for cargo (it rejects an older compiler), and the local checks run on the installed stable 1.96.0 without a download.
- **Owner review:** pending

## DL-003 — Claude permission mode names differ from PLAN §6.2

- **Context:** PLAN §6.2 lists the Claude modes `default` / `acceptEdits` / `plan` / `bypassPermissions`. `claude --help` of 2.1.289 lists `acceptEdits`, `auto`, `bypassPermissions`, `manual`, `dontAsk`, `plan`. There is no `default`.
- **Options:** (a) keep `default` and translate it; (b) use the native names of the installed CLI.
- **Choice:** (b). The S1 adapter offers exactly the native names (L8) and uses the most restrictive mode for an unknown stored mode. The capability matrix records the verified names. No PLAN decision changes: only the matrix value column changes.
- **Update (round 2):** live runs show that the wire name differs from the flag name. `--permission-mode manual` makes `system/init` report `"permissionMode":"default"`, and `--permission-mode default` is accepted as a hidden alias. The adapter therefore uses the wire names `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions` (one name for one meaning in events, the matrix, and `set_permission_mode`), and `default` is the safest mode. Evidence: fixture `spikes/s1-claude/fixtures/rust/set_permission_mode.jsonl`, test `adapter::tests::set_mode_accepts_only_native_modes`.
- **Reason:** L8 ("offer only the modes that the provider enforces natively"). A translated sentinel name is trap 1/2 of t3code-lessons §5.
- **Owner review:** pending

## DL-004 — The driven Claude session always passes `--permission-mode`

- **Context:** on this machine the user's Claude settings make `bypassPermissions` the default mode (a first live probe showed `"permissionMode":"bypassPermissions"` in `system/init`). A driven session that passes no mode inherits it.
- **Options:** (a) inherit the user's default; (b) always pass an explicit native mode, `manual` when none is stored.
- **Choice:** (b). `cli_args()` always adds `--permission-mode`.
- **Reason:** the goal forbids bypass mode outside a temporary directory, and L8 requires the most restrictive mode for an unknown stored mode. A station must not run with a mode that the owner did not choose.
- **Owner review:** pending

## DL-005 — Live recordings run with `--safe-mode`

- **Context:** the first live probe showed that the user's global Claude setup (v0 Fleet Deck hooks, plugins, `CLAUDE.md`, MCP servers) runs inside a driven session and adds `system/hook_*` frames and local paths to the output.
- **Options:** (a) record with the user's setup; (b) `--setting-sources` with a reduced set; (c) `--safe-mode` (no hooks, plugins, `CLAUDE.md`, MCP; authentication still works).
- **Choice:** (c) for `record-claude`. The scrubber also replaces the work directory, `$HOME`, and `$USER`, and redacts `account`, `email`, `organization`, and `messaging_socket_path`.
- **Reason:** fixtures must be reproducible on another machine and must not contain the owner's local setup. The product adapter (Phase 2) decides its own flags; `--safe-mode` is only a recorder choice.
- **Owner review:** pending

## DL-006 — "Prompt confirmed" for Claude is the `command_lifecycle` `started` frame

- **Context:** PLAN §6.4 needs a provider signal that moves an input from `claimed` to `confirmed`. A first adapter version started a turn on any `system/status` or stream frame; the round-1 review found that an unrelated frame could confirm a pending prompt.
- **Options:** (a) the first stream frame after a write; (b) the `isReplay` user echo of `--replay-user-messages`; (c) the `command_lifecycle` frame (`queued` → `started` → `completed`/`cancelled`/`discarded`/`refused`) that `claude` 2.1.289 emits for an input that carries a client `uuid`.
- **Choice:** (c). Each host input carries a client UUID; `started` with the same `command_uuid` confirms it and binds it to a turn. (b) stays only as a fallback for a CLI that sends no lifecycle frames. A turn that the CLI starts on its own (for example after a background task, trap 4) gets `origin: provider` and never confirms an input.
- **Reason:** (c) is the only signal that names the input. Evidence: fixtures `queued_turn`, `message_steering`, `turn_interrupt`; tests `adapter::tests::unrelated_frames_never_confirm_a_pending_prompt`, `provider_turn_does_not_confirm_a_pending_prompt`, `only_the_matching_uuid_confirms`.
- **Owner review:** pending

## DL-007 — Permission answers echo `toolUseID`, as the TypeScript SDK does

- **Context:** the SDK reference recordings (S1-3, `spikes/s1-claude/fixtures/sdk/`) show that `@anthropic-ai/claude-agent-sdk` 0.3.296 adds `toolUseID` (the `tool_use_id` of the `can_use_tool` request) to every permission answer. Our first adapter did not send it. The CLI accepted both forms in the recordings.
- **Options:** (a) keep the shorter answer; (b) send `toolUseID` when the request has a `tool_use_id`.
- **Choice:** (b). `control::permission_response` takes the request's `tool_use_id`. The Rust fixtures with approvals were re-recorded with the new frame.
- **Reason:** A8/D8 make the SDK the reference for the wire protocol. The field binds the answer to one tool call, so a late answer cannot apply to a different call. The deny answer keeps `interrupt: false`; the SDK omits it, and the CLI default is `false` (the reference replay treats both as equal).
- **Owner review:** pending

## DL-008 — SDK reference replay compares writes semantically

- **Context:** PLAN §14 S1 asks for "TS SDK frame recording + replay". SDK frames differ from ours in fields that do not change the CLI's behaviour: random request ids, `systemPrompt: [""]` in `initialize`, and no `uuid`/`session_id` on user messages.
- **Options:** (a) exact comparison (fails on every frame); (b) a semantic comparison of the behaviour-relevant fields, with the key differences reported as evidence.
- **Choice:** (b). `spikes/s1-claude/src/reference.rs` infers the host command from each SDK write, re-addresses CLI responses to the core's request ids, compares the projection of `reference::semantic`, and lists one-sided keys (`replay-claude --keys`).
- **Reason:** the replay must prove that the Rust core drives the CLI the same way as the reference client and handles every frame that the reference client receives (0 unknown frames). Exact bytes of ids are not part of the contract.
- **Consequence (evidence for DL-006):** the SDK sends no `uuid`, so the CLI sends no `command_lifecycle` frames and the SDK stream has no per-input confirmation. In the reference replay every turn is a provider-origin turn and no input reaches `started` (`tests/reference_fixtures.rs::sdk_prompts_are_never_confirmed`). The station adapter must send a `uuid` on every user message.
- **Owner review:** pending

## DL-009 — `allow_always` sends session-scoped allow rules only

- **Context:** a `can_use_tool` request carries `permission_suggestions` (fixture `rust/tool_call_approve.jsonl`): an `addRules` entry with `destination: "localSettings"`, an `addDirectories` entry, and a `setMode` entry (`acceptEdits`). The first adapter echoed all of them as `updatedPermissions` for `allow_always`. That writes `.claude/settings.local.json` into the owner's workspace, widens the workspace, and switches the session mode, all from one click. PLAN §11 lists "allow once / allow always / deny" but does not define the scope of "always".
- **Options:** (a) echo the suggestions unchanged; (b) send only the allow rules, re-scoped to `destination: "session"`; (c) no `allow_always` for Claude.
- **Choice:** (b). `adapter::map::session_rules` keeps `addRules`/`replaceRules` entries with `behavior: "allow"` and sets `destination: "session"`. It drops `setMode` (L8: a mode changes only through `set_permission_mode`) and `addDirectories`. Without an allow rule the request offers no `allow_always`. The option label is "Allow always (this session)".
- **Reason:** doctrine of least surprise and fail-closed permissions: a click in the deck must not change files in the workspace or the session mode without an explicit owner action. A persistent rule is a later, explicit feature (owner decision).
- **Owner review:** pending

## DL-010 — Two more `approval.expired` reasons: `turn_ended` and `cancelled_by_provider`

- **Context:** PLAN §6.3 lists the reasons `not_resumable` and `process_lost` for `approval.expired`. The Claude CLI also ends an open request in two other ways: it sends `control_cancel_request` for a request id, and a turn can end (`result`) while a request is still open (for example after an interrupt).
- **Options:** (a) map both to `process_lost`; (b) leave the request open; (c) add the reasons `cancelled_by_provider` and `turn_ended`.
- **Choice:** (c), in the spike canonical events. ADR-001 §2.4 and §2.9 document them. Phase 1 adds them to the `approval.expired` reason enum of the protocol crate.
- **Reason:** (a) gives a false cause in the timeline. (b) leaves an answerable card for a request that the CLI no longer waits for, and an answer to it would be a stale reply. A precise reason is consistent with L2 (show the real state).
- **Owner review:** pending

## DL-011 — Spike event `background.updated` for provider background work

- **Context:** PLAN §6.3 reports background work only as `run.completed { background_pending }` (L7). The Claude CLI reports the background set while a turn runs and after it (`system/background_tasks_changed`), and a background task or an async subagent can start a new turn without host input (traps 4 and 5).
- **Options:** (a) report only `background_pending` at turn end; (b) a spike event `background.updated { tasks }` each time the set changes.
- **Choice:** (b) in the spike only. The spike has no `run` entity; Phase 1 derives `run.completed.background_pending` from the same set and decides whether `background.updated` becomes a protocol event (an additive change).
- **Reason:** the evidence must show when the set drains (after the turn), which a turn-end flag cannot show. No PLAN decision changes.
- **Owner review:** pending
