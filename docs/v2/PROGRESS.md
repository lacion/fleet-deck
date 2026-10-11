# Fleet Deck v2 — progress (goal loop memory)

Current milestone: **M1 = PLAN Phase 0 (spikes S1, S2, S4)**. Max 15 rounds.

Rules for this file: one checklist item per M1 deliverable, in PLAN order. Each round updates the item status, the commands it ran, and the results. The work of a round stays uncommitted for the reviewer; the next round commits it first.

## M1 checklist

- [x] **P0-1 Scaffold.** Root Cargo workspace (spike members only, DL-001), `deny.toml`, lint policy (`unsafe_code = forbid`, `clippy::unwrap_used = deny` outside tests), `spike-canon` (canonical event subset of §6.3), `docs/v2/DECISIONS-LOG.md`, `spikes/README.md`, this file. *(round 1)*
- [~] **S1-1 Claude protocol core.** Frame parser (never panics, unknown frames kept, proptest), host control frames, sans-IO adapter core: init, prompt, partial deltas, `can_use_tool` with native option ids + edited input, idempotent answers, fail-closed unknown requests, interrupt, set mode (native modes only), exit expiry. *(round 1: core done; the live process driver moves to S1-2)*
- [ ] **S1-2 Claude live driver + fixture format + `record-claude`.** tokio process driver around the core; JSONL fixture format (`in`/`out`/`exit` records + header, scrubbed of account data and home paths); `record-claude` binary; live recordings (cheap model, temp dir) of the §13 shared scenarios `simple`, `multi_turn`, `queued_turn`, `turn_interrupt`, `message_steering`, `provider_thread_resume`, `tool_call_approve`, `tool_call_deny`, plus edited input, interrupt during a tool, set permission mode.
- [ ] **S1-3 TypeScript SDK reference recordings.** `spikes/s1-claude/sdk-recorder/` (bun + `@anthropic-ai/claude-agent-sdk`, a tee wrapper as `pathToClaudeCodeExecutable`); SDK fixtures for the same scenarios.
- [ ] **S1-4 Claude replay + evidence.** Replay harness (infers host commands from `in` records, compares the adapter's writes, maps request ids); `replay-claude` offline command; `insta` snapshots for every Rust and SDK fixture; evidence: "prompt confirmed", restart survival of open requests, native option ids, steer/queue timing.
- [ ] **S1-5 ADR-001** (Proposed): native Claude adapter design, effect states and crash boundaries (§6.4), fixture set, unsupported scenarios with reasons.
- [ ] **S2-1 Codex protocol core.** ADR-002 option evaluation (git pin of `codex-app-server-protocol` vs codegen from `codex app-server generate-json-schema`); JSON-RPC framing; sans-IO core: initialize, `thread/start`, `turn/start`, both approval requests, questions (L4), `turn/interrupt`, `thread/resume`, diff notifications.
- [ ] **S2-2 Codex live driver + `record-codex` + fixtures** for the §13 shared scenarios Codex supports (unsupported ones listed with reasons); restart survival experiment.
- [ ] **S2-3 Codex replay + `replay-codex` + ADR-002** (Proposed) with effect states and crash boundaries.
- [ ] **S4-1 Link identity + address rules.** Ed25519 identity keys, self-signed certs from them, mTLS with pinned-key verifiers, D13 address rules (loopback, RFC 1918, ULA, link-local, Tailscale ranges; reject public at bind and dial) as unit tests.
- [ ] **S4-2 Link handshake + channels.** `Hello` with `proto_min/max`, feature flags, roles, `link_epoch`, nonce; transcript signature bound to the TLS exporter; version negotiation; control channel (frames of §8.3) and data channel (multiplexed streams, credit flow control).
- [ ] **S4-3 Link pairing + reconnect.** `fdinv1:` invitations both directions (single use, 15 min, fingerprint-bound, secret proof bound to TLS); jittered backoff reconnect; duplicate link (`link_epoch`) rule; station-only-dials (NAT) case; multi-process loopback test with separate data dirs.
- [ ] **S4-4 ADR-004** (Proposed): link protocol v1 (frames, handshake, pairing, scopes, reconnect, feature flags).
- [ ] **M1-F Close-out.** PLAN §6.2 matrix: verified value + evidence per cell; `spikes/README.md` offline commands for every spike; full acceptance run; DECISIONS-LOG complete.

## Round log

### Round 1 (2026-10-11)

- Done: P0-1; S1-1 core (frames, control, adapter core + tests).
- Live probe (one run, haiku, temp dir): `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --model haiku`. Findings:
  - `initialize` control request → `control_response` with `commands`, `agents`, `models`, `account`, `current_permission_mode`, `capabilities`, ... (fixtures must scrub `account`).
  - `system/init` has `session_id`, `model`, `permissionMode`, `claude_code_version`, `tools`, `capabilities`.
  - The user's settings default to `bypassPermissions` → DL-004 (always pass `--permission-mode`).
  - The CLI modes are `manual|acceptEdits|plan|auto|dontAsk|bypassPermissions` → DL-003.
  - `result` has `terminal_reason` (`completed`), `stop_reason`, `usage`, `total_cost_usd`, `permission_denials`.
  - The user's global hooks run (`system/hook_*` frames): recordings should use `--safe-mode` or `--setting-sources` to keep fixtures free of local setup (decide in S1-2).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 19 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1 (no `web/`, no `ts-rs` types yet; Phase 1 adds them)
- Next: S1-2.
