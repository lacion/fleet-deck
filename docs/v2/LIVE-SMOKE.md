# Live smoke tests (M2 P5-5)

The normal checks (`make check`) are offline. They use fakes (`fake-claude`, `fake-codex`) and recorded fixtures. The live smoke tests are the only tests that start the real provider CLIs. Run them by hand when a provider CLI changes, or before a release.

## What the tests do

The file is `crates/fleetdeck-providers/tests/live_smoke.rs`. It has one test for each provider:

| Test | CLI | Mode | What it checks |
|---|---|---|---|
| `live_claude_smoke` | `claude` (with `--safe-mode`, DL-005) | `default` | The probe gives a version and the CLI is not signed out. A session starts in a new temporary directory. One turn ends `done`, the answer contains `pong`, and no approval is requested. A graceful stop works. |
| `live_codex_smoke` | `codex app-server` (effort `low`) | `untrusted/read-only` | The same checks for Codex. |

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
