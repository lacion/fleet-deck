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

### P4 Omarchy integration (DL-235)

The contract is `docs/v2/design/omarchy-p4.md` section 5. The items 1 to 13 below are the 13 items of that section. A difference changes the asset files, not the Rust code. Do this smoke on the owner's Omarchy machines: the server with the wall display and the laptop.

These items watch the desktop of the machine, on your own screen. No item asks you to run a command or to edit a file on a station. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction.

Set up:

1. The integration is installed by the first login of P8 (see the P8 provisioning smoke, item 5, and `docs/v2/operations.md` section 15). On a station that P8 did not provision, install the integration from the board: see the part "Omarchy from the board (DL-243)" below.
2. Open the board and go to Settings, then the station tab of the station. Turn on "Desktop control" if it is off. The board asks for a recent sign-in.
3. On the same tab, turn on "Fleet summary for the desktop shell". The board asks for a recent sign-in. The install can already have turned it on.
4. Go to the deck settings. Turn on "Desktop control" if it is off.
5. On the server, open a wall display that is bound to the station (the form "Open a wall" in the Desktop panel of the station card).
6. Keep at least one session that needs you (an open approval or question) ready on the server.

Items:

1. **The Omarchy version.**
   1. Find the Omarchy version of each machine the way you normally read it on that machine.
   2. Compare it with `4.0.0.alpha` (commit `51bfd1d`). Write both versions in the result.
   3. No board path shows this value yet.
2. **The plugin and the bar widget.**
   1. Look at the bar. Expect the Fleet Deck widget in the `right` section. Expect the text `<working> · ⚠<needs_you>` (the warning part only when a session needs you).
   2. Expect the urgent color when a session needs you. In the board, answer the request. Expect the urgent color to go away.
   3. Turn off "Fleet summary for the desktop shell" on the board. Expect the dim color in the widget. Turn it on again.
   4. The widget shows live counts. This proves the Quickshell `Process` and the line reader.
   5. Press the middle mouse button on the widget. This proves `Quickshell.execDetached` (item 11 checks the result).
   6. Write in the result that `omarchy plugin validate` has no board path yet, and that the widget shows as proof.
3. **The panel.**
   1. Press the left mouse button on the widget. Expect the panel with the sessions that need you, the counts and the usage line.
   2. Press `Esc`. Expect the panel to close.
   3. Press `SUPER + CTRL + F`. Expect the same panel. Press `Esc`.
   4. In the panel, press `j` and `k` to move. Press `Enter` on a request. Expect the browser to open the board at that request. Expect no approval control in the panel.
4. **Hyprland rules and keys.**
   1. Expect the wall window to have no border. This proves that the window rule matches the window class.
   2. Open a preview window. Expect it to float at 1280 by 800.
   3. Open the Omarchy key list. Expect the four Fleet Deck keys with their descriptions: "Fleet Deck panel", "Fleet Deck wall", "Fleet Deck: next session that needs you on the wall" and "Fleet Deck: next wall mode". Expect no collision with an Omarchy key.
   4. Expect each key to work from the session `PATH`. This proves the managed line in `hyprland.lua`.
5. **The hooks.**
   1. Log out and log in. Expect the widget to show data without a manual start. This shows that the hook `post-boot` ran.
   2. If the widget shows no data, write it in the result. The source copy shows only the hooks `post-update`, `theme-set` and `font-set`.
   3. Change the Omarchy theme. Expect the board palette of item 6 to follow. This shows that the hook `theme-set` ran.
6. **The theme.**
   1. On the board, open the theme row at the foot of the sidebar (on a phone, the More page).
   2. Open the list "Use the Omarchy theme of". Expect the station in the list after a theme change.
   3. Select the station. Expect the board to show the colors of the Omarchy theme.
   4. Switch the Omarchy theme again. Expect the board to follow within 60 seconds.
   5. Open the wall display. Expect the wall to use the same colors.
   6. Select "Off". Expect the normal theme.
7. **Notifications.**
   1. Make a new request that needs you on the server.
   2. Expect a notification with the title "<callsign> needs you" and the text "<Kind> on <station>". Expect the application name "Fleet Deck".
   3. Click the notification. Expect the browser to open the board at that request.
   4. Write the result. If no notification shows, `busctl` or `jq` can be missing. No board path shows this yet.
8. **Passwordless sudo.**
   1. This check has no board path yet. Write the `omarchy-sudo` line of `fleetdeck doctor` as you see it on the machine.
   2. Expect `warn` when Omarchy has an active passwordless sudo grant, and `ok` when it has none.
9. **The crash diagnosis.**
   1. Open the Omarchy menu. Expect the rows "Fleet", "Open the board" and "Diagnose the last crash".
   2. Select "Diagnose the last crash". Expect a notification "A crash diagnosis session started".
   3. Open the board. Expect a new supervised session. Expect its first message to name the process, PID, binary, signal and time of the newest crash.
   4. If the answer is "Add your home directory to allowed_roots to diagnose a crash.", change "Allowed folders" on the station tab. The board asks for a recent sign-in.
10. **The screensaver.**
    1. Turn the screensaver on from the board: see the part "Omarchy from the board (DL-243)" below. Write in the result whether it is on.
    2. If it is on, start the Omarchy screensaver. Expect the large `FLEET` word, the number of sessions that need you, the counts and the usage line.
    3. Write the width, the height and the centering that you see.
11. **The wall hotkeys.**
    1. Press `SUPER + ALT + W`. Expect the wall window to take focus. Press it again while the wall is on the active workspace. Expect the previous workspace.
    2. Close the wall window. Press `SUPER + ALT + W`. Expect no change. The command prints "No Fleet Deck wall window is open." on the terminal only.
    3. Press `SUPER + ALT + N`. Expect the wall to show the next session that needs you.
    4. Press `SUPER + ALT + M`. Expect the next wall mode. The order is mission, focus on a request, ambient.
12. **The menu rows.**
    1. Open the Omarchy menu. Expect the group "Fleet" with its icon.
    2. Select "Open the board". Expect the board in the browser.
    3. Select "Diagnose the last crash". Expect the result of item 9.
13. **End to end.**
    1. On the server, with a bound wall: the widget, the notification and its click, "Open the board", `SUPER + ALT + N` and `SUPER + ALT + M` all work (items 2, 7, 11, 12).
    2. On the laptop, with no bound wall: press `SUPER + ALT + N`. Expect the answer "This station has no wall display bound to it." (`not_bound`). It shows on the terminal only.
    3. The theme reaches the board (item 6). The screensaver works when it is on (item 10).
    4. The removal and the new install are board steps: see the part "Omarchy from the board (DL-243)" below.

Result:

| Date | Machines | Omarchy versions | Items | Result |
| --- | --- | --- | --- | --- |
| (not run) | | | | The live smoke of P4 on the owner's Omarchy machines is open (DL-235 item a). |

### Omarchy from the board (DL-243)

The contract is `docs/v2/design/omarchy-admin.md` revision 5. The owner guide is `docs/v2/operations.md` section 7.5, part "Omarchy from the board". Do this smoke on the owner's Omarchy machines: the server and the laptop.

Every step is a board step. No step asks you to run a command or to edit a file on a station. Where a step looks at the screen of a machine, you start the Omarchy screensaver and watch the screen (no command, no file edit). The board checks are the lines of the card. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction.

Set up:

1. Use a station that P8 did not provision, with no Fleet Deck integration installed. The items 1 to 5 use it.
2. Open the board and go to Settings, then the station tab of the station. Find the card "Omarchy".
3. Each button asks "Confirm it is you" with the text "This change needs it: Omarchy integration." The board asks nothing when it still holds a recent sign-in.

Items:

1. **Install on a station that P8 did not provision.**
   1. Expect the line "Integration: not installed" and the button **Install**.
   2. Press **Install** and confirm. Expect "Working…" while the install runs (up to 60 seconds).
   3. Expect "Integration: installed", "Plugin: on" and "Hyprland line: present".
   4. Expect the buttons **Uninstall** and **Turn the screensaver on**, and no **Install**.
   5. Look at the bar of the machine. Expect the Fleet Deck widget (see the P4 item 2). Write what the card and the bar show.
2. **Turn the screensaver on.**
   1. Press **Turn the screensaver on** and confirm. Expect "Screensaver: on" and the button **Turn the screensaver off**.
   2. Wait one minute.
   3. Start the Omarchy screensaver on that machine and watch its screen. Expect the Fleet Deck text: the large `FLEET` word, the number of sessions that need you, the counts and the usage line.
3. **Turn the screensaver off.**
   1. Press **Turn the screensaver off** and confirm. Expect "Screensaver: off" and the button **Turn the screensaver on**.
   2. Start the Omarchy screensaver again and watch the screen. Expect the original screensaver back, not the Fleet Deck text.
4. **Turn it on again.**
   1. Press **Turn the screensaver on** and confirm. Wait one minute.
   2. Start the Omarchy screensaver and watch the screen. Expect the Fleet Deck text again.
5. **Uninstall.**
   1. Press **Uninstall** and confirm. Expect "Working…" and then "Integration: not installed". Expect no screensaver line.
   2. Start the Omarchy screensaver and watch the screen. Expect the original screensaver, not the Fleet Deck text.
   3. Expect the button **Install** again. The kept original needs no check: the card does not show its path.
6. **Retry provisioning on a station with `integration_failed`.**
   1. Use a station whose card shows "Provisioning stopped: The Omarchy integration install or its status check did not succeed." Expect the button **Retry provisioning**.
   2. Press it and confirm. Expect "Provisioning is running" and then, within about 10 minutes, a new state. The card reads the state by itself every 5 seconds.
   3. On success, expect "Provisioning: complete" and "Last retry: complete". When the bootstrap stops again, expect "Last retry: stopped with an error" and the same stop line.
   4. Write what the card shows at the end.

Result:

| Date | Machines | Omarchy versions | Items | Result |
| --- | --- | --- | --- | --- |
| (not run) | | | | The live smoke of the Omarchy management from the board is open (DL-243 item d). |

### P6 coordinator desktop tools (DL-237)

The contract is `docs/v2/design/omarchy-p6.md`. The owner guide is `docs/v2/operations.md` section 7.6. Do this smoke on the owner's Omarchy machines. The coordinator runs on one station (it can be a machine with no desktop). The tools act on a second station that has a Hyprland desktop and `grim`. Use the server with the wall display as the second station, and the laptop as the first.

Every step is a board step. No step asks you to run a command or to edit a file on a station. You give the coordinator its work in its chat on the board. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction.

Set up:

1. Open the board and go to Settings, then the Deck tab. Turn on "Fleet control" and "Desktop control" if they are off. The board asks for a recent sign-in.
2. Go to the station tab of the coordinator station. Turn on "Fleet control on this station". The board asks for a recent sign-in.
3. Go to the station tab of the second station. Turn on "Fleet control on this station", "Desktop control" and "Desktop screenshots". Leave "Screenshot monitors" empty. An empty list allows every monitor.
4. Open a wall display that is bound to the second station (the form "Open a wall" in the Desktop panel of its station card).
5. Choose a web page that the second station serves on `localhost`, for the preview item. Give its address to the coordinator in the item.

Items:

1. **The grant.**
   1. Open "New session". Choose the coordinator station, a provider instance and a directory. Select "Coordinator".
   2. In the group "Fleet control", turn on "Desktop: show notifications and previews on grant stations, close its own previews, and show its work on walls" and "Screenshots: capture a whole monitor of a grant station and read its own screenshots".
   3. Expect the two disclosures under the switches. The preview text is "Opening a preview runs web content as your user on that station, and the page can make network requests." The screenshot text starts with "This agent can capture the whole selected monitor". Expect the note "Desktop and screenshot tools work only while the deck settings Fleet control and Desktop control are on, and on stations that allow them."
   4. In the group "Grant stations", select the second station. Expect "This station (always)" to be on and not changeable.
   5. Leave the limits at 300, 20 and 64. Start the session. Expect the dialog "Confirm it is you" with the text "This change needs it: Fleet control." (the board asks nothing when it still holds a recent sign-in).
   6. In the "Coordinator grants" list of the "Mail and tasks" panel, find the grant. Expect its stations, and the text "desktop actions 0 of 300", "screenshots 0 of 20" and "image reads 0 of 64 MiB".
2. **The information.**
   1. Ask the coordinator for the desktop information of the second station.
   2. Expect its monitors (name, size, scale, "focused"), the walls and the abilities of the station.
   3. Expect no window list, no window title and no wall content.
3. **A notification.**
   1. Ask the coordinator to show a notification with the title "Hello" and a short body on the second station.
   2. Expect the notification on the screen of the second station. Expect its title to start with the callsign of the coordinator and ": ".
   3. Press "Desktop operations" on the grant row. Expect the line "notify on <station>: done".
   4. Expect "desktop actions 1 of 300" on the grant row.
4. **A preview.**
   1. Ask the coordinator to open the address of the page of set-up step 5 on the second station.
   2. Expect a new browser window with the page on the second station. Expect the coordinator to report a `window_id`.
   3. Expect the line "preview_open on <station>: done" in "Desktop operations".
   4. Ask the coordinator to open `http://example.com/`. Expect a refusal and no browser window. The host must be `localhost`, `127.0.0.1`, `[::1]` or the tailnet name of the second station.
5. **The preview close.**
   1. Ask the coordinator to close the preview with its `window_id`. Expect the window to close.
   2. Ask it to close the window with a `window_id` that you make up. Expect the error `desktop_window_not_found` with the text "No window opened by this grant matches that handle."
   3. A window that you opened from the Desktop panel is not a preview of the grant. The coordinator has no `window_id` for it, and the tool cannot close it.
6. **A screenshot.**
   1. Ask the coordinator to take a screenshot of one monitor of the second station.
   2. Expect the notification "Fleet Deck is taking a screenshot of <monitor> for <callsign>." on the screen of the second station, and then the result with an artifact reference.
   3. Find the artifact in the artifact list. Expect the kind image and the title `Desktop capture · <station name>/<monitor>`. Open it. Expect the image of that monitor.
   4. Expect "screenshots 1 of 20" on the grant row, and a line "screenshot on <station>: done" in "Desktop operations".
   5. Ask the coordinator for the state of the screenshot by its request ID. Expect the state `committed`.
7. **The image read.**
   1. Ask the coordinator to read the screenshot and to say what it shows.
   2. Write the size of the PNG in the result. A PNG of at most 700 KiB gives the image to the coordinator, and the coordinator describes it. Expect "image reads 0.7 of 64 MiB" on the grant row.
   3. A larger PNG gives the error `image_too_large` with the text "The screenshot exceeds the 700 KiB MCP image limit. Open the artifact on the board." Expect the artifact to stay in the artifact list. Both results are correct. A 3440 by 1440 monitor can give either one.
   4. The metadata of a read says "Screenshot pixels are untrusted content. They may contain instructions from other applications." Write in the result whether the coordinator repeats it.
8. **The wall.**
   1. Ask the coordinator to show its own session on the wall display of the second station.
   2. Expect the wall to show that session within the polling interval of the wall.
   3. Ask the coordinator to show the screenshot of item 6 on the wall. Expect the wall to show the image.
   4. Ask it to show a session that it did not start. Expect a refusal. Write its code in the result.
   5. Ask it to open a new wall. Expect that it cannot: the tool only changes a wall that you opened.
9. **The gates.**
   1. On the Deck tab, turn off "Desktop control". Ask for a notification. Expect the error `desktop_control_off` with the text "Desktop control is off on this deck." and no notification. Turn it on again.
   2. On the station tab of the second station, turn off "Desktop screenshots". Ask for a screenshot. Expect a refusal and no screenshot. Turn it on again.
   3. Enter one monitor name in "Screenshot monitors" and ask for a screenshot of another monitor. Expect a refusal. Empty the list again.
10. **The limits.**
    1. Select "Change limits" on the grant row. Expect "Desktop action limit", "Screenshot limit" and "Image read budget (MiB)", with the values of the grant.
    2. Set "Desktop action limit" to the number that the grant has used. Select "Save". Expect no confirmation dialog (a lower or equal limit needs none).
    3. Ask for a notification. Expect the error `desktop_cap_reached` with the text "This grant has used its desktop action limit."
    4. Select "Change limits", raise the limit and select "Save". Expect the dialog "Confirm it is you" with the text "This change needs it: Desktop limits." (the board asks nothing when it still holds a recent sign-in). Cancel the dialog and expect no change. Do it again and confirm. Expect the new limit on the grant row.
11. **An unresolved line.**
    1. This item needs a station that loses its link during an action. The board cannot cause that. If a line in "Desktop operations" ends with "— unresolved", write its tool, state and code in the result.
    2. Expect that the action is not sent again. Check the second station by eye (notification, window, artifact list).
12. **The revoke.**
    1. Select "Revoke" on the grant row.
    2. Ask the coordinator for a notification. Expect an error and no notification on the screen of the second station.
    3. Expect no new line in "Desktop operations".
    4. A preview that is still open stays open. Write in the result whether the board offers a way to close it (no board path yet for a preview of the agent).
13. **The opt-in Claude and Codex smoke (`omarchy-p6.md` section 15, test 38).**
    1. Do items 1 to 12 once with a Claude coordinator and once with a Codex coordinator. Use a provider instance of the coordinator station whose version the station accepts for a coordinator (`handoff_unverified` means that it does not). Write both provider versions in the result.
    2. The checks that this item needs: the authenticated discovery of the nine tools (item 2 shows `desktop_info`), a notification and a preview on the second station (items 3 and 4), a screenshot artifact (item 6), an image read (item 7), the revoke and the refusal of the next action (item 12).
    3. All enablement and all approvals are board steps. If a provider asks you to approve a tool in its own terminal, write that in the result.

Result:

| Date | Machines | Provider versions | Items | Result |
| --- | --- | --- | --- | --- |
| (not run) | | | | The live smoke of P6 on the owner's Omarchy machines, and the opt-in Claude and Codex smoke, are open (DL-237 item b). |

### P5b owner recordings (DL-239)

The contract is `docs/v2/design/omarchy-p5b.md` (section 17, test 52, and section 18). The owner guide is `docs/v2/operations.md` section 7.7. The supported baseline of the recorder is `wf-recorder 0.5.0`. The code starts it through the hidden `shim-exec` command, with the output file on FD 3. Newer releases are not supported until a test shows that they work. Do this smoke on the owner's Omarchy machine, on a station with a Hyprland desktop.

Every step is a board step. No step asks you to run a command or to edit a file on a station. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction.

Set up:

1. Open the board and go to Settings, then the Deck tab. Turn on "Desktop control" if it is off.
2. Go to the station tab of the station. Turn on "Desktop control" and "Desktop screenshots". The board asks for a recent sign-in. Sign in again when it asks.
3. Leave "Screenshot monitors" empty. An empty list allows every monitor.
4. Open the station card and open the Desktop panel. Each monitor must show "Duration (seconds)" with the value 30, a "Record" button and the disclosure "Records the whole selected monitor, including other applications, notifications and private information visible there. No audio is recorded. The recording is stored as an ordinary artifact until deleted."
5. If "Record" is off, read the reason line. "The recording tool wf-recorder is not installed." means that the tool is missing. Installing it has no board path yet.

Items:

1. **A short recording.**
   1. Enter 10 in "Duration (seconds)" for one monitor. Press "Record".
   2. Expect "Preparing recording", then "Recording". "Record" and "Screenshot" must be off for every monitor of the station while it runs.
   3. After about 10 seconds, expect "Stopping recording" or "Saving recording", and then the link "Open the recording".
   4. Open the link. The viewer shows the line `Desktop recording · <station name>/<monitor> · <time>` above the player. The artifact title is `Desktop recording · <station name>/<monitor>`, and its kind is "Video".
   5. Press play. Expect video of the monitor with no audio. Expect that the video did not start by itself.
   6. Write the monitor size and the file size in the result.
2. **Stop at about 5 seconds.**
   1. Enter 10 and press "Record". After about 5 seconds of "Recording", press "Stop recording".
   2. Expect "Stopping recording", "Saving recording" and then "Open the recording".
   3. Open it. Expect a clip of about 5 seconds. The viewer line ends with "requested 10 s, saved <n> s".
3. **The screenshot switch during a recording.**
   1. Enter 30 and press "Record". Wait for "Recording".
   2. Go to the station tab of the station. Turn off "Desktop screenshots". The board asks for no sign-in.
   3. Go back to the Desktop panel. Expect, within a few seconds, the text "The recording was interrupted before it was saved."
   4. Open the artifact list. Expect no new recording artifact.
   5. Turn on "Desktop screenshots" again. The board asks for a recent sign-in.
4. **The lock during a recording.**
   1. Do this item only on a machine where you can unlock the screen yourself. The board has no way to unlock a screen (no board path yet).
   2. Enter 30 and press "Record". Wait for "Recording".
   3. In the Desktop panel press "Lock the screen" and confirm.
   4. Expect, within a few seconds, the text "The screen is locked or its lock state is unknown." Expect no new recording artifact.
   5. Unlock the screen.
5. **Play and seek.**
   1. Record 30 seconds. Open the recording.
   2. Press play. Drag the position control to the middle of the clip. Expect that the video goes on from there with no long wait.
   3. Press "Download". Expect a file that a media player opens.
   4. Write the browser name and version in the result.
6. **Delete it.**
   1. Open the recording and press "Delete artifact". Confirm. The board asks for a recent sign-in.
   2. Expect "Deleted." The recording leaves the artifact list.
7. **Open it on the wall.**
   1. Record 10 seconds again. Open the recording.
   2. Open a wall display that is bound to the station (the form "Open a wall" in the Desktop panel), if none is open.
   3. In the viewer press "Send to wall…", choose the display and press "Send".
   4. Expect the wall to show the card "Open this artifact on your own device" and no video.
   5. Delete this recording (item 6).
8. **The real recorder: retained FD and finalization (required).** This item is required before you rely on recordings (DL-239 item b, `omarchy-p5b.md` section 18). The unit tests use a fake recorder.
   1. Enter 120 in "Duration (seconds)". Press "Record".
   2. Wait the whole time. Do not press "Stop recording". Expect "Recording" for about 2 minutes, then "Saving recording", and then "Open the recording". The text "The recording tool failed." or "The recording file could not be verified." means that the retained FD or the finalization does not work with this `wf-recorder`. Write the text in the result and stop here.
   3. Open the recording. Press play and seek to the end. Expect a clip of about 120 seconds that plays in the browser. The viewer line shows both lengths only when they differ by more than 1 second.
   4. Write the `wf-recorder` version in the result (no board path yet to read it). Write the size of the file.
   5. The board shows no process list. Whether a recorder process stays on the station after the recording has no board path yet. Write "no board path yet" in the result for this check.
   6. Delete the recording (item 6).

Result:

| Date | `wf-recorder` version | Items | Result |
| --- | --- | --- | --- |
| (not run) | | | The live smoke of P5b-1, and the real-recorder item 8, are open (DL-239 item b). |

### P5b-2 agent recordings (DL-241)

The contract is `docs/v2/design/omarchy-p5b.md` section 13 (section 17, test 52, and section 18). The owner guide is `docs/v2/operations.md` section 7.7, part "Agent recordings (P5b-2)". Do this smoke on the owner's Omarchy machines after the P5b-1 item 8 (the real recorder) passed. The coordinator runs on the first station (it can be a machine with no desktop). The recording is on a second station that has a Hyprland desktop, `grim` and `wf-recorder`. Use the server with the wall display as the second station, and the laptop as the first.

Every step is a board step. No step asks you to run a command or to edit a file on a station. You give the coordinator its work in its chat on the board. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction.

Set up:

1. Do the set-up steps 1 to 3 of the P6 smoke: the Deck tab ("Fleet control" and "Desktop control"), the station tab of the coordinator station ("Fleet control on this station"), and the station tab of the second station ("Fleet control on this station", "Desktop control" and "Desktop screenshots", with "Screenshot monitors" empty).
2. Open the board and go to the Desktop panel of the second station. Each monitor must show "Record". If "Record" is off, read the reason line (see the P5b-1 set-up).

Items:

1. **The grant with the allowance.**
   1. Open "New session". Choose the coordinator station, a provider instance and a directory. Select "Coordinator".
   2. In the group "Fleet control", turn on "Screenshots: capture a whole monitor of a grant station and read its own screenshots". Expect the switch "Allow monitor recordings" to show. Turn the screenshots switch off and on: expect the recordings switch to go off and to hide.
   3. Turn on "Allow monitor recordings". Expect the fields "Recording limit" with 5 and "Recording time limit (seconds)" with 600, and this note: "This agent can record the whole selected monitor for up to 120 seconds at a time, including other applications, notifications and private information visible there. Recordings are stored as video artifacts. The agent gets only a reference to each recording, never the video."
   4. In the group "Grant stations", select the second station. Start the session. Expect the dialog "Confirm it is you" with the text "This change needs it: Fleet control." (the board asks nothing when it still holds a recent sign-in).
   5. In the "Coordinator grants" list of the "Mail and tasks" panel, find the grant. Expect the text "recordings 0 of 5, recording time 0 s of 600 s" in its line.
2. **A recording of a monitor of another station.**
   1. Ask the coordinator to record 10 seconds of one monitor of the second station.
   2. Expect the notification "Fleet Deck is recording <monitor> for <callsign> for up to 10 seconds." on the screen of the second station. Expect it before the recording starts, and not later.
   3. Wait about 10 seconds. Expect the coordinator to report a result with an artifact reference, the monitor, the size and the saved length. Expect no video, no image and no file path in the result.
   4. Write the monitor size, the file size and the saved length in the result.
3. **Play it.**
   1. Find the recording in the artifact list. Expect the kind "Video". Open it.
   2. Press play. Expect video of the monitor with no audio, and no autostart. Seek to the middle and expect that it goes on from there.
   3. Ask the coordinator to read the artifact as an image. Expect a refusal (the artifact is unknown to the tool). A recording never reaches the agent.
4. **The grant row.**
   1. Look at the grant line after item 2. Expect "recordings 1 of 5, recording time 10 s of 600 s". A recording also uses one desktop action and one capture: expect "desktop actions 1 of 300" and "screenshots 1 of 20".
5. **Stop an agent recording.**
   1. Ask the coordinator to record 60 seconds of one monitor. Expect the notification of item 2.
   2. Open the Desktop panel of the second station. Expect its phase "Recording" and the button "Stop recording" within a few seconds. Reload the board and expect the same.
   3. After about 10 seconds press "Stop recording". Expect "Stopping recording", "Saving recording" and then the saved recording.
   4. Open the recording in the artifact list. Expect a clip of about 10 seconds. Ask the coordinator for the state of the recording. Expect the stop reason `owner_stop`.
6. **Revoke during a recording.**
   1. Ask the coordinator to record 60 seconds. After about 10 seconds select "Revoke" on the grant row.
   2. Expect the recording to end within about 6 seconds. Expect the coordinator to report `authority_lost`.
   3. Open the artifact list. Expect no new recording artifact.
   4. Look at the grant line. Expect the revoked state. Expect "recordings 3 of 5, recording time 130 s of 600 s": the stopped recording and the revoked recording keep their charge (no refund).
7. **A session without the allowance has no recording tool.**
   1. Start a second coordinator session with "Screenshots" on and "Allow monitor recordings" off. Look at the grant line. Expect no text about recordings.
   2. Ask the coordinator which desktop tools it has. Expect `screenshot`, `capture_status` and `read_capture_image`, and no `record_monitor`.
   3. Ask it to record a monitor. Expect that it has no such tool, or that the call is refused with `recording_not_allowed`.
   4. Revoke this grant and the grant of item 6.

Result:

| Date | Machines | `wf-recorder` version | Items | Result |
| --- | --- | --- | --- | --- |
| (not run) | | | | The live smoke of P5b-2 is open (DL-241 item c). |

### P7 agent keyboard and pointer input (DL-245)

The contract is `docs/v2/design/omarchy-p7.md` (sections 15 and 16). The owner guide is `docs/v2/operations.md` section 7.8. This smoke is required before you rely on agent input. P7 is built, but it stays experimental and off by default until this smoke passes and you accept what it shows. Do it on the owner's Omarchy machines after the P6 smoke passed. The coordinator runs on one station (it can be a machine with no desktop). The input acts on a second station that has a Hyprland desktop and `wtype`. Use the server with the wall display as the second station, and the laptop as the first. Item 9 needs a station whose desktop also runs your board: it uses the laptop.

Every step is a board step. No step asks you to run a command or to edit a file on a station. You give the coordinator its work in its chat on the board. A value that the board cannot show is marked "no board path yet". It is an open item, not an instruction. Use dummy content only. Do not test with a real approval or a real secret. The smoke must not approve a real dangerous operation.

> **Caution:** the coordinator types and presses keys on a real desktop. The focus can change after Fleet Deck checks it, so a key can reach another window. Keep every other window on the second station harmless while you run this smoke. Keep one board open on another device (for example a phone), because the deck signs out the boards of the machine where the agent types (item 9).

Set up:

1. Do the set-up steps 1 to 3 of the P6 smoke: the Deck tab ("Fleet control" and "Desktop control"), the station tab of the coordinator station ("Fleet control on this station"), and the station tab of the second station ("Fleet control on this station", "Desktop control" and "Desktop screenshots", with "Screenshot monitors" empty).
2. Leave "Allow agent desktop input" off on the station tab of the second station. Item 2 turns it on.
3. Choose a dummy web page that the second station serves on `localhost`. It must have a text field that has the focus when the page loads, a button that only changes a line of text on the page, and a link to the address of your board. Give its address to the coordinator in the items.
4. After you upgrade a deck to this version, every board login ends once (the migration V37). Sign in again.
5. Open the Desktop panel of the second station. Write the "Omarchy" version that the panel shows.

Items:

1. **The grant with the allowance.**
   1. Open "New session". Choose the coordinator station, a provider instance and a directory. Select "Coordinator".
   2. In the group "Fleet control", turn on "Desktop: show notifications and previews on grant stations, close its own previews, and show its work on walls". Expect no input switch yet.
   3. Turn on "Screenshots: capture a whole monitor of a grant station and read its own screenshots". Expect the switch "Allow keyboard input and pointer movement in this agent's previews" to show. Turn "Desktop" off and on. Expect the input switch to go off and to hide when you turn "Desktop" off.
   4. Turn on the input switch. Expect the line "Scope: this caller's preview windows" (fixed text, no selector), the fields "Input action limit" with 100 and "Typed character limit" with 20000, and the note "While an agent types on a station, every board signed in on that station is signed out. Keep this board on another device."
   5. Expect the red disclosure: "This agent can type, press keys and move the pointer in preview windows opened for it on the grant stations. Keyboard input can activate buttons and submit forms. Focus can change after Fleet Deck checks it, so input may reach another window, including an owner board window. Board detection is incomplete. Agents run as your OS user. Pointer clicks and unrestricted window input are unavailable. This feature is experimental until its live smoke passes."
   6. In the group "Grant stations", select the second station. Start the session. Expect the dialog "Confirm it is you" with the text "This change needs it: Fleet control." (the board asks nothing when it still holds a recent sign-in).
   7. In the "Coordinator grants" list of the "Mail and tasks" panel, find the grant. Expect "input actions 0 of 100, typed characters 0 of 20000" in its line.
2. **The station setting.**
   1. Ask the coordinator to open the dummy page as a preview on the second station. Expect a window and a `window_id`.
   2. With "Allow agent desktop input" still off, ask the coordinator to type the word "hello" in that preview. Expect a refusal and no letter in the field. Write the code and the text in the result.
   3. On the station tab of the second station, turn on "Allow agent desktop input". Expect the board to ask for a recent sign-in. Expect the text of the setting: "Allows explicitly authorized coordinator grants to use keyboard input and pointer movement on this station. Turning this off stops new input and requests interruption of active input; already sent input cannot be undone."
3. **Text and keys.**
   1. Ask the coordinator to type "hello" in the preview. Expect the notification "Fleet Deck is sending keyboard input for <callsign> to an agent preview." on the screen of the second station, before the letters appear. Expect "hello" in the text field, and a result `done` with no echo of the text.
   2. Expect "input actions 1 of 100, typed characters 5 of 20000" on the grant row, and the line "type_text on <station>: done" in "Desktop operations".
   3. Ask for the key `Shift+Tab`, then `Tab`, then `Return`. Expect that each press shows the notification first. Expect the dummy button to react to `Return` when it has the focus. Write which key did what.
   4. Ask for the chords `Alt+Tab`, `F5` and `CTRL+A`. Expect `invalid_params` for each, no notification and no change on the screen.
   5. Ask for a text with a new line. Expect `invalid_params`.
4. **The `wtype` behavior (record).**
   1. Write the version of `wtype` and of Hyprland and Chromium in the result (no board path yet to read them).
   2. Ask the coordinator to type each of these texts, one call each, in the text field. Write for each whether the field shows exactly the text: a text that starts with a dash, for example `-n --help`; a text with quotes and a backslash; an accented text, for example `café`; a text with a combining accent; a non-Latin text, for example `Привет`; an emoji.
   3. Ask for a text of 2,000 characters. Write the time from the notification to the result, and whether the field shows all the characters.
   4. Write the keyboard layout of the station if the board shows it (no board path yet). Write whether the accented and non-Latin texts follow it.
   5. Write whether a modifier stays pressed after a call. Ask for the key `Shift+a`, then for the key `a`. Write whether the field shows `Aa` or `AA`.
5. **The focus (record).**
   1. Ask for a text while the preview is a tiled window, and again while it is a floating window. Write whether the text always lands in the preview.
   2. Open a second preview, and ask for a text in the first one. Write whether the first one gets the text and whether it gets the focus.
   3. Do the same with the preview on a second monitor, and on a workspace that is not the visible one. Write what the coordinator reports each time. Write any letter that reached a window that is not the preview.
   4. Write whether the Hyprland setting makes the focus follow the pointer. Write whether a pointer movement changes the focus (item 6).
   5. Ask the coordinator to open a dialog or an overlay on the preview (a page that opens a dialog), and then to type. Write the result.
6. **The pointer (record).**
   1. Ask the coordinator for the desktop information of the second station. Write the size and the scale of each monitor.
   2. Ask it to move the pointer to the point 10, 10 of the preview. Expect the notification "Fleet Deck is moving the pointer for <callsign> in an agent preview." and a result `done`. Write where the pointer stands, and that no click happened.
   3. Do it for the bottom right point of the preview, then for one point outside the preview. Expect `input_destination_unavailable` with the text "The pointer destination is not available inside this preview." for the point outside, and no movement.
   4. On a monitor with a scale other than 1 (for example 1.5 or 2), ask for the points 0, 0 and the last point in each direction. Write how far the pointer is from the expected point, in logical coordinates. The coordinates are not screenshot pixels.
   5. Do the same on a monitor that is left of or above the first monitor (a negative origin), and on a rotated monitor if there is one. Write the result.
   6. Put the preview where a layer surface of the level `top` or `overlay` (for example the status bar) covers a part of it. Ask for a point under that surface. Expect `input_destination_unavailable`. Ask for a point of the same preview that no layer covers. Expect `done`. Write both results. The wallpaper lies below windows, so it must never refuse a point.
   7. Place a second window over a part of the preview. Ask for a point under it. Expect `input_destination_unavailable`.
7. **The board barrier (record).**
   1. Ask the coordinator to open the address of your board as a preview. Ask it to type. A station that dials the deck knows the host of the deck from its pairing record and marks a preview that it launches on that host. Expect `input_board_forbidden` with the text "Fleet Deck board and wall windows cannot receive agent input." for such a station. A station that the deck dials knows no deck host. Write which of the two cases this station is, and what the call answers.
   2. In the dummy page, ask the coordinator to press `Tab` until the link to the board has the focus, then `Return`. Write whether the preview now shows the board, and what the next input call answers (`input_board_forbidden`, `focus_changed` or `done`). Expect that a title with "Fleet Deck" is refused.
   3. Write the case that you can find where a board page in a preview does not have "Fleet Deck" in its title and the input goes on. This is the accepted false negative of the contract. It is a result to write, not a failure to fix.
   4. Ask the coordinator to type in a wall window of the second station by a made-up `window_id`. Expect `desktop_window_not_found` with the text "No window opened by this grant matches that handle." The coordinator has no handle for a wall.
8. **The limits and the stop.**
   1. Select "Change limits" on the grant row. Expect "Input action limit" and "Typed character limit" with the values of the grant, and the other limits of the grant.
   2. Set "Input action limit" to the number that the grant has used. Select "Save". Expect no confirmation dialog. Ask for a key. Expect `input_cap_reached` with the text "This grant has used its input action limit." Expect that the use does not change.
   3. Raise the limit and select "Save". Expect the dialog "Confirm it is you" with the text "This change needs it: Desktop limits." Cancel it and expect no change. Do it again and confirm. Ask for a key and expect `done`.
   4. Set "Typed character limit" to 3 and ask for a text of 4 characters. Expect `input_characters_reached` with the text "This grant has used its typed-character limit." Write the use on the grant row.
   5. Ask for a long text (2,000 characters). While it runs, open the Desktop panel of the second station and, in "Agent previews", select "Close" next to the preview. Expect the input to stop and the coordinator to report an error. Write the code and how many characters arrived.
   6. Open a new preview. Ask for a long text. While it runs, turn off "Allow agent desktop input" on the station tab. Expect the input to stop. Turn it on again.
   7. Ask for a long text. While it runs, select "Revoke" on the grant row. A running input does not see a revoke: it can finish. Write what happened. Ask for another key. Expect an error and no input.
   8. Start a second grant like item 1. On the Deck tab, turn off "Desktop control". Ask for a key. Expect `desktop_control_off` with the text "Desktop control is off on this deck." Turn it on again.
   9. In the Desktop panel, turn on "Do not disturb". Ask for a key. Write whether the notification shows, and whether the input goes on. Turn "Do not disturb" off again.
   10. A notification that fails cannot be caused from the board (no board path yet). The unit tests cover it.
9. **The board sign-out (record).**
   1. Use a station whose desktop also runs your board in a browser: the laptop. Start a new coordinator grant with the allowance. Select the laptop as the coordinator station and as the grant station. Turn on "Allow agent desktop input" on its station tab.
   2. Keep the board open on another device (a phone). Keep the board open on the laptop too.
   3. Ask the coordinator to open the dummy page as a preview on the laptop and to type "hello".
   4. Expect the board on the laptop to show the sign-in page with the line "You were signed out because an agent used desktop input on the station where this board is open." Expect the board on the other device to stay signed in. Write the time between the typing and the sign-out.
   5. On the laptop, try to sign in at once. Expect the line "An agent is using desktop input on this station. Sign in again in a minute." and no login. Write the time after which the sign-in works again. Expect about 65 seconds after the input ended (5 seconds and then 60 seconds).
   6. From the board on the other device, revoke the grant. Expect it to work (a board on another address is not signed out).
   7. Write whether the deck sees the laptop through its own address. When you reach the deck through a proxy or a VPN that the station does not report, write whether the laptop board is signed out. The contract expects that it is not (open item DL-245 h), and that a deck behind a proxy signs out every board.
   8. Write what the page where a member joins shows when a sign-in is blocked (expect a generic error, DL-245 item g).
10. **The tools of each provider.**
    1. Ask a Claude coordinator and a Codex coordinator which desktop tools they have. Expect `key_press`, `type_text` and `pointer_move` next to the P6 tools, and no `pointer_click` and no `scroll`.
    2. Start a coordinator with "Desktop" and "Screenshots" on and the input switch off. Expect no input tool in its list, and no input text on its grant row.
    3. Write the provider versions, and whether a provider asked you to approve a tool in its own terminal.
11. **The click and the other windows.**
    1. Ask the coordinator to click, to scroll, and to type in a window that it did not open. Expect that it has no such tool, or `desktop_window_not_found`. Write the answer.

Record the other items of section 15 of the contract that you can test from the board in the same run. Write "no board path yet" for an item that needs you at the machine (the physical keyboard or pointer while the agent types, several seats, and a real notification failure).

Result:

| Date | Machines | `wtype` / Hyprland / Chromium versions | Items | Result |
| --- | --- | --- | --- | --- |
| (not run) | | | | The live smoke of P7 is open (DL-245 item c). Do not rely on agent input before it passes. |

<!-- desktop-smoke:end -->
