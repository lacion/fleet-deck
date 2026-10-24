# Fleet Deck v2 — Phase 0 spikes

These crates de-risk PLAN §14 Phase 0. They never ship. The product crates (Phase 1) take the proven parts into `crates/`.

| Spike | Directory | Proves | ADR |
| --- | --- | --- | --- |
| shared | `spike-canon/` | Spike subset of the canonical events (PLAN §6.3) | — |
| S1 | `s1-claude/` | Claude Code stream-json control protocol from Rust (A8, D8) | `docs/v2/adr/ADR-001-native-claude-adapter.md` (Proposed) |
| S2 | `s2-codex/` | Codex app-server JSON-RPC from Rust | [`docs/v2/adr/ADR-002-native-codex-adapter.md`](../docs/v2/adr/ADR-002-native-codex-adapter.md) |
| S4 | `s4-link/` | Link protocol v1 (mTLS, pairing, channels, reconnect) | [`ADR-004`](../docs/v2/adr/ADR-004-link-protocol-v1.md) |

## Offline commands (no network, no provider CLI)

All commands in this section also run in a sandbox without network and without sockets (checked with `codex sandbox -P :workspace -C . -- <command>`, DL-025):

- `cargo deny check` reads the advisory database from `target/advisory-dbs` (`deny.toml`). On a fresh clone, run `cargo deny fetch` once with network access.
- The s4-link tests use TCP on `127.0.0.1` where the OS allows IP sockets. Where it does not, `.cargo/config.toml` (`FD_LINK_TRANSPORT=auto`) moves the same loopback addresses to FIFO pairs under `<tmp>/fd-link-net`. To force a transport, set `FD_LINK_TRANSPORT=tcp` or `FD_LINK_TRANSPORT=fifo:<dir>`.

**One command for the whole offline acceptance run** (DL-026). It runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, the workspace tests, `cargo deny check`, `replay-claude` and `replay-codex`, prints each exit code, and exits non-zero if one fails:

```sh
sh spikes/acceptance.sh
FD_TEST_RUNNER=test sh spikes/acceptance.sh   # use `cargo test` instead of cargo-nextest
```

The workspace tests run with cargo-nextest when it is installed, else with `cargo test --workspace`. Both runners run the same 240 tests (`CI=true`, so a snapshot mismatch fails). For Codex reviewers, the project rules file `.codex/rules/fleet-deck-acceptance.rules` allows these offline commands, and only these (no recorder, no live CLI). Check it with `codex execpolicy check -r .codex/rules/fleet-deck-acceptance.rules cargo nextest run --workspace`.

Run all spike tests from the repository root:

```sh
cargo nextest run --workspace
```

S1 unit and property tests (frame parser, control frames, adapter core):

```sh
cargo nextest run -p s1-claude
```

S1 offline replay of every recorded fixture (`spikes/s1-claude/fixtures/rust/*.jsonl`). It rebuilds each session in the adapter core and fails when a host write differs from the live run:

```sh
cargo run -p s1-claude --bin replay-claude
cargo run -p s1-claude --bin replay-claude -- --events spikes/s1-claude/fixtures/rust/message_steering.jsonl
cargo nextest run -p s1-claude --test replay_fixtures
```

S1 reference replay of the frames that the TypeScript Agent SDK recorded (`spikes/s1-claude/fixtures/sdk/*.jsonl`, D8). The replay infers each host command from the SDK's write, applies it to the adapter core, and compares the core's write semantically (DL-008). `replay-claude` with no path replays both directories:

```sh
cargo run -p s1-claude --bin replay-claude -- spikes/s1-claude/fixtures/sdk --keys
cargo nextest run -p s1-claude --test reference_fixtures
```

S1 canonical event snapshots (one `insta` snapshot per fixture in `spikes/s1-claude/tests/snapshots/`) and the adapter evidence tests that `spikes/s1-claude/EVIDENCE.md` cites:

```sh
CI=true cargo nextest run -p s1-claude --test event_snapshots --test evidence --test capabilities --test matrix --test fixture_hygiene
```

`capabilities` and `matrix` hold the PLAN §6.2 capability-matrix evidence (EVIDENCE.md E7; every test name is cited in the matrix). `fixture_hygiene` fails if a fixture contains a local path (home, user name, temporary directory), also across streamed deltas.

`CI=true` makes `insta` fail on a changed snapshot instead of writing a `.snap.new` file. After an intended change, review the new streams with `INSTA_UPDATE=always cargo nextest run -p s1-claude --test event_snapshots` and `git diff spikes/s1-claude/tests/snapshots`.

SDK recorder self-test (offline: a fake `claude` that never answers). It proves that the recorder exits nonzero on an unknown or timed-out scenario and kills the timed-out CLI process:

```sh
spikes/s1-claude/sdk-recorder/selftest.sh
```

### S2 (Codex)

JSON-RPC framing, the sans-IO adapter core, and the check of every method name against the vendored app-server schema of codex-cli 0.160.0 (`spikes/s2-codex/schema/0.160.0/`, DL-012):

```sh
cargo nextest run -p s2-codex
```

Replay every recorded Codex fixture (`spikes/s2-codex/fixtures/*.jsonl`) through the core. Each host write must match the recording, and no frame may be unknown:

```sh
cargo run -p s2-codex --bin replay-codex                    # all fixtures
cargo run -p s2-codex --bin replay-codex -- --events spikes/s2-codex/fixtures/simple.jsonl
```

The same checks and the adapter evidence (prompt confirmation, restart survival, native option ids, held interrupt) run as tests:

```sh
cargo nextest run -p s2-codex --test replay_fixtures --test evidence --test matrix --test fixture_hygiene --test schema_methods --test schema_conformance
```

Codex supports all 8 shared scenarios of PLAN §13 (`simple`, `multi_turn`, `queued_turn`, `turn_interrupt`, `message_steering`, `provider_thread_resume`, `tool_call_approve`, `tool_call_deny`). No shared scenario is unsupported. Two need a note:

- `queued_turn`: the host holds the second prompt and sends it after the first turn, because `turn/start` during a turn joins that turn (DL-015).
- `tool_call_deny`: under `untrusted` Codex 0.160.0 offers `accept`, `acceptWithExecpolicyAmendment` and `cancel` for a command, and no `decline`. The recorder denies with `cancel` (DL-015).

Extra Codex fixtures: `question_rpc` (L4 RPC question), `plan_capture`, `file_change`, `interrupt_during_tool`, `interrupt_right_after_send` (trap 3), `set_mode`, `approval_restart` (open approval across a kill and `thread/resume`), `fork_thread` (`thread/fork`), `subagent` (collab agent with a child thread), `background_terminal` (a command that outlives the turn, found by `thread/backgroundTerminals/list`). `matrix` holds the PLAN §6.2 matrix evidence for Codex.

The second L4 question channel (an `agentMessage` with `delivery: "async"` questions, answered by a new user message) has no live fixture: in 0.160.0 its tool is behind the under-development feature `send_message_to_user_async`, and the model cannot call it. The unit test `async_questions_are_answered_by_a_new_message` and the schema test `async_question_fields_exist_in_the_schema` cover it (ADR-002 §2.6).

### Fixture format

One JSON object per line. Line 1 is the header `{"fixture":{"provider","scenario","cli_version","recorder","model","notes"}}`. Each next line is a record with a `dir` tag:

| `dir` | Meaning |
| --- | --- |
| `spawn` | The host started a CLI process with `args` (a new process of the same session after a restart). |
| `host` | A host command (`initialize`, `prompt`, `answer`, `interrupt`, `set_mode`, `set_model`). |
| `in` | A frame that the host wrote to the CLI stdin. |
| `out` | A frame that the CLI wrote to stdout. |
| `raw` | A stdout line that is not JSON. |
| `exit` | The process ended (`code` is `null` when the host killed it). |
| `note` | A recorder note (timings, observations). |

The recorder scrubs the work directory (`<workdir>`), `$HOME` (`<home>`), `$USER` (`<user>`), and the keys `account`, `email`, `organization`, `messaging_socket_path`.

### S4 (link)

S4 needs no provider CLI and no recordings: every test runs real TLS 1.3 handshakes between generated identities on `127.0.0.1` (TCP, or FIFO pairs on a host without sockets; see above).

```sh
cargo nextest run -p s4-link
```

What runs so far (S4-1):

- `src/addr.rs` unit and property tests: the D13 ranges (loopback, RFC 1918, ULA, link-local, Tailscale `100.64.0.0/10` and `fd7a:115c:a1e0::/48`), IPv4-mapped IPv6, refusal of `0.0.0.0`/`::` and public addresses at bind, dial and accept (DL-017). A real tailnet check is an owner step.
- `src/identity.rs`: Ed25519 identity, self-signed certificate, fingerprint, key file with mode 600.
- `tests/tls.rs`: pinned-key mTLS in both directions (station dials, deck dials), refusal of unpinned listeners and dialers, wrong role, revocation on the next handshake (no TLS session resumption, DL-017), missing client certificate, fingerprint policy, a forged certificate that embeds a pinned key, and the address refusal before any packet.

S4-2 adds the link itself (`src/hello.rs`, `src/frames.rs`, `src/mux.rs`, `src/link/`):

- `src/hello_tests.rs`: version negotiation (highest common version, clear error without one), feature intersection with required features, unknown `Hello` fields ignored, identity/role/channel/nonce checks, and transcript signatures bound to the TLS exporter, both hellos and the side.
- `src/frames.rs`: control frames (§8.3), the 1 MiB limit, and the method table with scopes and feature gates (DL-019).
- `src/mux.rs`: data-channel streams with credit flow control (64 KiB chunks, 256 KiB window), including a property test.
- `tests/link.rs`: both dial directions with requests in both directions, events/acks and ping; version mismatch reported on both sides; feature-gated methods and channels; scope checks, narrowing and revocation; link epochs (higher supersedes, lower or equal refused); a data channel without its control channel; 1 MiB each way over the data channel; a reader that gives no credit stops the writer.

```sh
cargo nextest run -p s4-link --test link
```

S4-3 adds pairing, reconnect and several processes (`src/pairing.rs`, `src/link/pair.rs`, `src/reconnect.rs`, `src/bin/fd-link-node.rs`):

- `tests/link.rs` (also): closing the control channel ends the data channel on both sides; a superseded link loses its data channel; dropping the data handles keeps the link up.
- `tests/pairing.rs`: `fdinv1:` invitations in both directions (a station without a listener joins the deck and dials it; the deck joins a station invitation and dials the station), single use, expiry on both sides, the invited key only, a wrong secret, unpaired keys limited to the pair channel, and the reconnect supervisor (backoff with jitter, a higher link epoch after the deck comes back).
- `src/pairing.rs` and `src/reconnect.rs` unit tests: invitation text format, proofs, failure cap, backoff bounds and reset, growing epochs (DL-021, DL-022).
- `tests/multiprocess.rs`: separate `fd-link-node` processes with their own data directories and ports on loopback: two NAT stations (no listener, dial only) pair and dial the deck, a used invitation fails, the deck restarts on the same port and data directory and both stations reconnect with a higher epoch; the mirror direction (station invites, deck joins and dials); public and wildcard addresses refused.

```sh
cargo nextest run -p s4-link --test pairing --test multiprocess
```

`tests/timeouts.rs` (DL-023): a peer that completes TLS and then stalls never blocks a dial, a pairing, the reconnect supervisor, or the listener; each fails with the reason `timeout` after `NodeConfig::handshake_timeout`.

```sh
cargo nextest run -p s4-link --test timeouts
```

The whole S4 spike runs offline with `cargo nextest run -p s4-link`. ADR-004 maps every §14 S4 row to its tests.

## Live commands (owner or recorder only)

Live recordings use the real `claude` and `codex` CLIs. They run in a temporary directory with a tiny prompt and a cheap model. They are not part of the offline checks.

```sh
cargo run -p s1-claude --bin record-claude -- --scenario all      # or one scenario name
```

`record-claude` runs `claude` 2.1.x with `--model haiku --safe-mode --permission-mode default` (DL-004, DL-005) in a `tempfile` directory and writes `spikes/s1-claude/fixtures/rust/<scenario>.jsonl`. A failed scenario writes `<scenario>.failed.jsonl`; the tests skip it, and it must be deleted before a commit.

The TypeScript SDK reference recorder (spike only, never ships) runs the same scenarios through `@anthropic-ai/claude-agent-sdk`. `tee-claude.mjs` sits between the SDK and `claude` as `pathToClaudeCodeExecutable` and logs every frame:

```sh
cd spikes/s1-claude/sdk-recorder && bun install && bun record.ts all   # or one scenario name
```

It writes `spikes/s1-claude/fixtures/sdk/<scenario>.jsonl` with the same header, records, and scrubbing rules. SDK fixtures have no `host` records. `approval_restart` and `interrupt_during_tool` exist only as Rust fixtures: they test our driver's process control (a kill with an open request, an interrupt during a long tool), not the SDK's frames.

The Codex recorder drives `codex app-server` 0.160.x with `gpt-5.6-luna`, effort `low`, and mode `untrusted/workspace-write` in a `tempfile` directory. It writes `spikes/s2-codex/fixtures/<scenario>.jsonl` (DL-014: it uses the user's `~/.codex`, so the scrubber removes account data and local paths):

```sh
cargo run -p s2-codex --bin record-codex -- --scenario all      # or a comma list
```

- `s-move/`: FG-9 spike, a Claude and a Codex conversation resume by native id on another machine in another folder (ADR-010).
