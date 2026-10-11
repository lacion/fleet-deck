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

The replay commands for the recorded fixtures are added when the fixtures exist (see `docs/v2/PROGRESS.md`).

## Live commands (owner or recorder only)

Live recordings use the real `claude` and `codex` CLIs. They run in a temporary directory with a tiny prompt and a cheap model. They are not part of the offline checks.
