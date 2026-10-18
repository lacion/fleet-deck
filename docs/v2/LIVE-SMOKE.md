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

<!-- desktop-smoke:begin -->
## Desktop (R13)

The desktop smoke list needs a real Hyprland desktop, a signed-in board and one station on that desktop. Run `make desktop-smoke` to print this list. The command runs nothing else.

Every step is a board step. No step needs you at the station. The station can be a remote machine.

### P5a: owner screenshots and artifact deletion (DL-233)

The supported baseline of the screenshot tool is `grim 1.5.0`. The code calls `grim -t png -s 1 -l 6 -o <monitor> -`, and `grim` writes the PNG to its standard output.

Set up:

1. Open the board and go to Settings, then the station tab of the station.
2. Turn on "Desktop control" if it is off.
3. Turn on "Desktop screenshots". The board asks for a recent sign-in. Sign in again when it asks.
4. Leave "Screenshot monitors" empty. An empty list allows every monitor.
5. Open the station card. The Desktop panel must show a "Screenshot" button for each monitor.

Items:

1. **Each monitor.**
   1. Press "Screenshot" for each monitor, one monitor at a time.
   2. Expect an image in the Desktop panel and an "Open the screenshot" link.
   3. Open the link. The viewer shows the same image.
   4. Find the artifact in the artifact list. Its title is `Desktop capture · <station name>/<monitor>`.
   5. Write the `grim` version of the station in the result.
2. **A scaled monitor.**
   1. Press "Screenshot" for a monitor with a scale other than 1.
   2. Expect an image. Write the image size in the result.
   3. The error "The monitor exceeds the screenshot size limit." is correct in one case only. One side is more than 8192 pixels, or the monitor has more than 16,777,216 pixels.
3. **A monitor with DPMS off.**
   1. Press "Screen off" for one monitor and confirm.
   2. Press "Screenshot" for the same monitor.
   3. Write the result in the table. The board must show an image or an error text within 45 seconds.
   4. Turn the monitor on again from the Desktop panel.
4. **The lock state (`hyprlock`).**
   1. Check that the Desktop panel shows "Locked" as "off".
   2. Press "Screenshot". Expect an image. This proves that `locked == Some(false)` on the real desktop.
   3. Do not lock the screen of a remote station for this smoke. Two station tests cover the locked case: `a_locked_or_unknown_lock_state_before_the_capture_refuses_it` and `a_screen_that_locks_during_the_capture_discards_the_output`.
5. **A stalled `grim`.**
   1. The station kills a stalled `grim` and all its child processes as one group. The board cannot start a stall.
   2. If one screenshot ends with an error, press "Screenshot" again at once.
   3. Expect an image, and not the error "A capture is already running.". This shows that the station freed its capture slot.
   4. The test `capture_through_shim_exec_kills_a_stalled_group_at_the_timeout` covers the group kill.
6. **A remote station, from the board.**
   1. Use a station that is on another machine.
   2. Press "Screenshot" in its Desktop panel. Expect the image.
   3. Find the artifact in the artifact list.
   4. Open the artifact and press "Delete artifact". Confirm.
   5. Expect "Deleted." The artifact leaves the artifact list.
   6. The board can show "Hidden on this deck. Waiting for station deletion." In that case, wait. The station deletes its copy when its link is up. The board then shows "Deleted."

Result:

| Date | `grim` version | Items | Result |
| --- | --- | --- | --- |
| (not run) | | | The live smoke of P5a on the owner's Omarchy machine is open (DL-233 item d). |

### P8 provisioning (DL-234)

Do this smoke after the release task of the owner. The release task supplies the pinned `fleetdeck-bin` package, its SHA-256, the trusted `/etc/fleetdeck-provision-release.json` and the installer image step. Before that task, a binary run of the bootstrap stops at `release_untrusted`, and P8-11 puts the real end-to-end run here.

The steps are in `docs/v2/operations.md` section 15. After the first login of the new machine, every step is a board step. No step needs you at the new machine.

Set up:

1. On your deck machine, make the file: `fleetdeck deck provision-file <name> --deck <tailnet name>:7421 --out <private folder>/provision.toml`.
2. Put the file on a supported medium as `fleetdeck/provision.toml`.
3. Make sure that the new machine has its own Tailscale enrollment key and its own disk-encryption credential.

Items:

1. **A fresh machine from the medium.**
   1. Install a fresh machine with the trusted installer image and the medium.
   2. Expect the file `<config home>/fleetdeck/provision.toml` on the new machine, made by the image.
   3. Log in. Expect no terminal window from the hook.
2. **The notification.**
   1. Expect a notification "Fleet Deck pairing" with the text "Approve this station fingerprint on the board:" and 16 groups of four characters.
   2. Open the board. Stations, "Add station", "A computer on my tailnet", then "Add" on the row of the new machine.
   3. Expect the request card "<name> asks to join this deck" with the text "Compare all 16 groups with the fingerprint shown on the new machine. Approve only if every group matches."
   4. Compare the 16 groups on the card with the 16 groups in the notification. Write in the result that every group matched.
3. **The approval.**
   1. Select "Yes, the codes match. Add <name>". Sign in again when the board asks.
   2. Expect the station on the board with its link up.
4. **The providers.**
   1. Open the station card. Expect the provider versions of the station.
   2. A provider that is signed out is a warning only. A provider that is `broken` or `unknown` stops the bootstrap with `provider_probe_failed`.
5. **The Omarchy integration.**
   1. The bootstrap installs the Omarchy integration and checks its status as its last step.
   2. Open the station card on the board. Expect the Desktop panel of the station. Write in the result what the board shows.
6. **A refused pairing asks again.**
   1. Use a machine that you have not approved yet.
   2. When the request card shows, select "No, refuse".
   3. Wait 10 minutes. Expect a new request card with the same 16 groups. Open the pairing window again when the board shows that it is closed.
   4. Approve it. Expect the station on the board.
7. **A clone test.**
   1. Install two machines from one medium. Use a distinct Tailscale enrollment key and a distinct disk-encryption credential for each machine.
   2. Expect two different fingerprints in the two notifications and on the two request cards.
   3. Write both fingerprints in the result.
Result:

| Date | Machines | Items | Result |
| --- | --- | --- | --- |
| (not run) | | | The live smoke of P8 waits for the release task of the owner (DL-234 item b). |

Task P4 I adds its items to this section later.
<!-- desktop-smoke:end -->
