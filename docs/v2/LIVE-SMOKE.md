# Live smoke tests (M2 P5-5)

The normal checks (`make check`) are offline. They use fakes (`fake-claude`, `fake-codex`) and recorded fixtures. The live smoke tests are the only tests that start the real provider CLIs. Run them by hand when a provider CLI changes, or before a release.

## What the tests do

The file is `crates/fleetdeck-providers/tests/live_smoke.rs`. It has one test for each provider:

| Test | CLI | Mode | What it checks |
|---|---|---|---|
| `live_claude_smoke` | `claude` (with `--safe-mode`, DL-005) | `default` | The probe gives a version and the CLI is not signed out. A session starts in a new temporary directory. One turn ends `done`, the answer contains `pong`, and no approval is requested. A graceful stop works. |
| `live_codex_smoke` | `codex app-server` (effort `low`) | `untrusted/read-only` | The same checks for Codex. |
| `claude_and_codex_authenticate_to_a_loopback_mcp_endpoint_with_the_env_handoff` | `claude` (with `--strict-mcp-config`, `--disable-slash-commands`), then `codex` | Claude `default`; Codex `never/read-only` | The coordinator credential handoff, provider half (ADR-006 §8.5, DL-219, DL-220). A loopback MCP probe records the digest of each `Authorization` header. Each session finds the probe tool, calls it exactly once and repeats its answer. Every request carries the handed-off credential, and the credential is in no command line and in no provider settings file. A pass sets the minimum version in `fleetdeck_providers::coordinator::VERIFIED`. |
| `fleetdeck-station` `manager::coordinator_live_tests::a_live_claude_coordinator_binds_and_calls_the_station_ingress_once`, `a_live_codex_coordinator_binds_and_calls_the_station_ingress_once` | `claude`, `codex` through the station | as above | Test 31, station half (DL-220): a deck start with a grant through the real station path and the station MCP ingress. The binding is pending, then active at generation 1 with the native id of `session.started`; exactly one tool call reaches the forward with that binding; stop revokes it; the credential is in no command line, station store file or provider settings file. Run: `FD_LIVE_SMOKE=1 cargo test -p fleetdeck-station --lib -- --ignored --nocapture --test-threads 1 coordinator_live_tests`. |

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
