# Fleet Deck v2 — Phase 0 spikes

These crates de-risk PLAN §14 Phase 0. They never ship. The product crates (Phase 1) take the proven parts into `crates/`.

| Spike | Directory | Proves | ADR |
| --- | --- | --- | --- |
| shared | `spike-canon/` | Spike subset of the canonical events (PLAN §6.3) | — |
| S1 | `s1-claude/` | Claude Code stream-json control protocol from Rust (A8, D8) | `docs/v2/adr/ADR-001-*.md` |
| S2 | `s2-codex/` (planned) | Codex app-server JSON-RPC from Rust | `docs/v2/adr/ADR-002-*.md` |
| S4 | `s4-link/` (planned) | Link protocol v1 (mTLS, pairing, channels, reconnect) | `docs/v2/adr/ADR-004-*.md` |

## Offline commands (no network, no provider CLI)

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
CI=true cargo nextest run -p s1-claude --test event_snapshots --test evidence
```

`CI=true` makes `insta` fail on a changed snapshot instead of writing a `.snap.new` file. After an intended change, review the new streams with `INSTA_UPDATE=always cargo nextest run -p s1-claude --test event_snapshots` and `git diff spikes/s1-claude/tests/snapshots`.

SDK recorder self-test (offline: a fake `claude` that never answers). It proves that the recorder exits nonzero on an unknown or timed-out scenario and kills the timed-out CLI process:

```sh
spikes/s1-claude/sdk-recorder/selftest.sh
```

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
