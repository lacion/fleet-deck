# Live smoke tests (M2 P5-5)

The normal checks (`make check`) are offline. They use fakes (`fake-claude`, `fake-codex`) and recorded fixtures. The live smoke tests are the only tests that start the real provider CLIs. Run them by hand when a provider CLI changes, or before a release.

## What the tests do

The file is `crates/fleetdeck-providers/tests/live_smoke.rs`. It has one test for each provider:

| Test | CLI | Mode | What it checks |
|---|---|---|---|
| `live_claude_smoke` | `claude` (with `--safe-mode`, DL-005) | `default` | The probe gives a version and the CLI is not signed out. A session starts in a new temporary directory. One turn ends `done`, the answer contains `pong`, and no approval is requested. A graceful stop works. |
| `live_codex_smoke` | `codex app-server` (effort `low`) | `untrusted/read-only` | The same checks for Codex. |
| `live_codex_shell_smoke` | `codex app-server` (effort `low`) | `untrusted/read-only` | The `!` path of DL-196 (B8) on the real CLI. No model call happens. `session.shell_command` sends `thread/shellCommand` for `pwd && echo fd-shell-ok`. The events come in order: `run.started` (origin `shell`, summary `!pwd && echo fd-shell-ok`), `tool.started` (node `shell-e-live-1`, kind `shell`), `tool.output` (the marker and the temporary directory), `tool.completed` (`ok`, exit code 0, `provider_refs.shell` = `done`), `turn.completed`, `run.completed`. The run has no assistant message. A second command, `exit 3`, ends `tool.completed` `failed` with exit code 3. Run: `FD_LIVE_SMOKE=1 cargo test -p fleetdeck-providers --test live_smoke -- --ignored --nocapture live_codex_shell`. |
| `claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff` | `claude` (with `--strict-mcp-config`, `--disable-slash-commands`), then `codex` | Claude `default`; Codex `never/read-only` | The coordinator credential handoff, provider half (ADR-006 §8.5, DL-219, DL-220). A loopback MCP probe records the digest of each `Authorization` header. Each session finds the probe tool, calls it exactly once and repeats its answer. Every request carries the handed-off credential, and the credential is in no command line and in no provider settings file. A pass sets the minimum version in `fleetdeck_providers::coordinator::VERIFIED`. |
| `fleetdeck-station` `manager::coordinator_live_tests::a_live_claude_coordinator_binds_and_calls_the_station_ingress_once`, `a_live_codex_coordinator_binds_and_calls_the_station_ingress_once` | `claude`, `codex` through the station | as above | Test 31, station half (DL-220): a deck start with a grant through the real station path and the station MCP ingress. The binding is pending, then active at generation 1 with the native id of `session.started`; exactly one tool call reaches the forward with that binding; stop revokes it; the credential is in no command line, station store file or provider settings file. Run: `FD_LIVE_SMOKE=1 cargo test -p fleetdeck-station --lib -- --ignored --nocapture --test-threads 1 coordinator_live_tests`. |
| `fleetdeck-cli` `live_bridge::claude_passes_the_token_to_the_stdio_bridge`, `codex_passes_the_token_to_the_stdio_bridge` | `claude`, `codex` | Claude `default`; Codex `never/read-only` | The stdio bridge of a shim host (DL-194 §9.10). A stub station socket records the digest of each `AUTH` line. The provider starts `fleetdeck mcp-bridge` through a wrapper that records the digest of `FLEETDECK_COORDINATOR_TOKEN`, calls the `fleet_ping` tool exactly once and repeats its answer. Every `AUTH` carries the minted token, and the token is in no command line and no file of the temporary directory. A control round without the token forwarding must show no `seen` record and no authenticated connection. A pass sets the minimum version in `fleetdeck_providers::coordinator::BRIDGE_VERIFIED`. Run: `make smoke-live-bridge`. |

The prompt is `Reply with the single word pong. Do not use any tool.` Each step has a time limit of 180 s. The temporary directory is removed after the test.

## Requirements

- The `claude` and `codex` CLIs are on `PATH` and signed in. To use other binaries, set `FD_CLAUDE_BIN` and `FD_CODEX_BIN`.
- Network access to the provider.

> **Caution:** the tests use your provider accounts and spend a small number of tokens.

## How to run the tests

1. Run both tests:

   ```sh
   make smoke-live
   ```

2. To run one provider only, use the test name:

   ```sh
   FD_LIVE_SMOKE=1 cargo test -p fleetdeck-providers --test live_smoke -- --ignored --nocapture live_codex
   ```

## Why the tests do not run by default

- Each test has `#[ignore]`, so `cargo test`, `cargo nextest run` and `make check` skip it.
- Each test also does nothing unless `FD_LIVE_SMOKE=1`. A run with `--ignored` and without the flag prints `skipped` and passes.

## Last result

| Date | Claude | Codex | Result |
|---|---|---|---|
| 2026-10-11 | 2.1.289 (`oauth_token`) | 0.160.0 (`chatgpt`) | Both passed (`make smoke-live`, 7.1 s). |
| 2026-10-12 | 2.1.296 (`oauth_token`) | not run (no coordinated start) | Coordinator handoff test passed for Claude (34 s, DL-219). |
| 2026-10-12 | 2.1.296 (`oauth_token`) | 0.160.0 (`chatgpt`) | Coordinator handoff test 31 passed for both providers: provider half (49 s) and station half `coordinator_live_tests` (46 s), DL-220. |

`live_codex_shell_smoke`: 2026-10-16, pass on Linux (WSL2) with Codex 0.162.1.

`live_bridge` (DL-194 §9.10): 2026-10-16, pass on Linux (WSL2) with Claude 2.1.301 and Codex 0.162.1, both control rounds included. `BRIDGE_VERIFIED` lists Claude 2.1.296 and Codex 0.162.1.
