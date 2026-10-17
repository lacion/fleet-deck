# Try Fleet Deck v2 on your machines

This guide is for the owner's first hands-on tests of the `v2` branch. It
uses the default directories of your user. For a throwaway run, add
`--data-dir <dir>` to every `fleetdeck` command (or set
`FLEETDECK_DATA_DIR`).

> **Caution:** Fleet Deck v1 (the npm package, port 4711) and v2 use
> different ports and directories, so both can run at the same time. Do not
> point v2 at the data of v1.

## 1. Build

You need Rust stable, `bun` and `git`. Sign in to `claude` and/or `codex`
first: the station uses the CLIs and their logins as they are.

```sh
git clone https://github.com/lacion/fleet-deck && cd fleet-deck
git switch v2
bun install --cwd web --frozen-lockfile
bun run --cwd web build          # the board files; the binary embeds them
cargo build --release -p fleetdeck-cli
export PATH="$PWD/target/release:$PATH"
fleetdeck --version
```

Build the board before the binary. A binary built without `web/dist`
shows "This deck has no board files" in the browser.

## 2. One machine

1. Start the deck and a local station in one process:

   ```sh
   fleetdeck up
   ```

   It prints `board at https://127.0.0.1:7420`. Keep it running.
2. In a second terminal, get a login link:

   ```sh
   fleetdeck open
   ```

   It prints the SHA-256 fingerprint of the self-signed board certificate
   and a single-use link that is valid for 5 minutes.
3. Open the link. The browser warns once about the certificate. Accept it
   only if the fingerprint matches.
4. Optional: set an owner password for later logins: `fleetdeck deck password`.

With no `station.toml`, the station may run sessions anywhere in your home
directory and offers the `claude` and `codex` instances. Configuration
files are in `~/.config/fleetdeck/` and must have mode 0600.

## 3. What to try on the board

| Flow | Steps |
| --- | --- |
| Spawn | **New session** → station, provider, browse to a directory, optional mode, model and first prompt → **Start session**. |
| Approval | Ask the agent to run a shell command. The request shows in the **Needs you** rail and in the session. Answer it there. |
| Steer and follow-up | While a turn runs, type a message and use **Steer now** or **Queue follow-up**. |
| Interrupt | **Interrupt** during a turn. The timeline shows the result. |
| Mode and model | The mode ladder and the model field in the session view. Only the controls that the provider supports show. |
| Subagents | Ask Claude to use a subagent. The session shows the subagent tree; the card shows the running-subagent count. |
| Deck restart | Stop `fleetdeck up` (Ctrl-C) and start it again. The board shows stale data, then reconnects without a new login. |
| Work without the deck | `fleetdeck station sessions`, `fleetdeck station run --provider claude --cwd <dir> "<prompt>"`, `fleetdeck station approve <request>`. These need only the station. |

## 4. Two machines

Machine A runs the deck (and its own station). Machine B runs a station
only. The machines must reach each other on a private LAN or on a tailnet.

> **Caution:** use only loopback, private LAN (10/8, 172.16/12, 192.168/16)
> or tailnet (100.64/10) addresses. The deck refuses a public address. Do
> not forward a port from a router.

### Machine A (deck)

1. Write `~/.config/fleetdeck/deck.toml` (mode 0600). Use the LAN or
   tailnet address of machine A:

   ```toml
   listen = ["127.0.0.1:7420", "192.168.1.10:7420"]   # board
   link_listen = ["192.168.1.10:7421"]                 # stations dial here
   # tailnet_hostname = "deck.tailnet-name.ts.net"    # if you open the board by that name
   ```

2. `chmod 600 ~/.config/fleetdeck/deck.toml`, then `fleetdeck up`.
3. In a second terminal: `fleetdeck deck invite`. Copy the invitation. It
   is single-use and secret.

### Machine B (station)

1. Optional: write `~/.config/fleetdeck/station.toml` (mode 0600), for
   example `name = "laptop"` and `allowed_roots = ["~/code"]`.
2. Start the station: `fleetdeck station serve`. Keep it running.
3. In a second terminal: `fleetdeck station join`, and paste the
   invitation at the prompt. Do not put it on the command line.
4. The station dials the deck. On the board, **Stations** shows it as
   **Link up** with its version, capacity and providers.

The pairing stays after restarts. To pair again: `fleetdeck station
forget-deck` on B, or `fleetdeck deck remove-station <name>` on A.

### Open the board from a phone or the other machine

Open `https://192.168.1.10:7420` (or the tailnet name). Run
`fleetdeck open` on machine A for a login link and change its host to the
address that you open, or log in with the owner password.

## 5. Optional checks

- Full test suite (offline, about 10 minutes): `make check`.
- Live smoke tests against the real CLIs (network, a few tokens):
  `make smoke-live`. See [LIVE-SMOKE.md](./LIVE-SMOKE.md).
- Logs: `FLEETDECK_LOG=debug fleetdeck up`.

## 6. macOS before a release

GitHub CI runs only on demand (DL-158), so the owner runs the checks on a macOS machine before a release. The behavior of setup cleanup, launchd, socket paths and process groups is different on macOS. Do these steps on the Mac, in this order:

1. `make check`. It runs the full suite on macOS. Read these results with care:
   - the shim tests (`shim_*`): process groups and the shim host;
   - `setup_tests.rs` and `setup_cleanup.rs`: the cleanup of a setup command uses `ps -E` on macOS and has no group check (DL-148);
   - the `doctor` and `station_*` tests that start real processes.
2. `make package-check`. It checks the launchd agents in `packaging/launchd/` (`dev.fleetdeck.*.plist`) statically.
3. Install the agents by hand as `packaging/README.md` says (copy the plists to `~/Library/LaunchAgents`, then `launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.fleetdeck.station.plist`). Run `fleetdeck doctor`, then `launchctl bootout` the agent. The station agent must use `AbandonProcessGroup`, so that shim-hosted agents survive a station stop (operations guide).
4. Use a short data directory when a socket test fails with "path too long". A Unix socket path is limited to about 104 bytes on macOS (108 on Linux): use `--data-dir /tmp/fd` for the check.
5. `make board-repro`. It uses `shasum` on macOS.
6. `make smoke-live` with signed-in `claude` and `codex` CLIs.
7. `fleetdeck doctor` on the Mac. It must not show `warn provider ... is older than the tested ...` for a CLI that you just installed.

`make perf` reads `/proc` and runs on Linux only. Do not run it on macOS.

## 7. Rules for tests

These rules are for every person and every agent that changes this repository.

- **Green or revert.** A change is complete only when `make check` is green. If a change makes a check fail and you cannot fix the cause now, revert the change. Do not leave a red check on `v2`.
- **Never edit, skip or ignore a test to make it pass.** Do not add `#[ignore]`, `skip`, `todo`, `.only`, a wider timeout, a looser assertion or a deleted test to get a green result. If a test is wrong, the owner decides, and the change that fixes the test gets its own DL entry that says why the old assertion was wrong.
- **No lane may run zero tests.** `tools/check.sh` fails when nextest runs fewer than 1000 tests or `bun test` runs fewer than 300 (R8.4). Raise a floor when the suite grows. Never lower it to make a run pass.
- **Fail-open behaviors must not get stricter.** An agent can make code look more correct and make it less fail-open. The behaviors below keep a session or the owner working when something fails. Each has a test. A change that makes one of them stricter (a new refusal, a new wait, a new error) needs an owner decision first.

| Fail-open behavior | Where it is decided | Test |
| --- | --- | --- |
| A deck or network failure never stops a session (A10, doctrine rule 2). | PLAN A10 | `a_command_for_an_unreachable_station_waits_and_is_sent_when_it_links` |
| A probe that fails reports `unknown` sign-in, never "signed out", and never opens a session (L11, trap 9). | `crates/fleetdeck-providers` probes | `probe_of_a_missing_binary_reports_unknown`; `station_info_providers_are_probed_once_with_versions` |
| A CLI that is older than the tested version only warns. Only a version on the deny list is refused (DL-158). | `fleetdeck-providers` `version.rs` | `version::tests::an_older_version_is_older_than_tested`; `a_start_on_a_broken_version_is_refused_with_the_reason` |
| A checkpoint never delays or fails a turn. A directory outside Git, a submodule or a queued input gives an explicit `unavailable` side, not an error (DL-084). | `manager/turns.rs`, `workspace/boundary.rs` | `a_directory_outside_git_finalizes_every_turn_as_not_a_git_worktree`; `a_native_queued_turn_gives_explicit_unavailable_sides`; the R8.5 bar in `tests/perf.rs` |
| A station restart retires the effects of a dead process and expires its approvals as `not_resumable`; it never replays an effect (A10, L2, L3). | `store/recovery.rs` | `a_lost_process_retires_queued_effects_and_expires_its_requests` |
| Auto-continue is off by default, and an uncertain recovery stays `lost` (L19). | `manager/auto_continue.rs` | `auto_continue_tests.rs` |
| An event or a field from a newer peer is kept as unknown and written back unchanged (L16). | `fleetdeck-proto`, `fleetdeck-link` | `an_event_of_a_newer_build_is_kept_as_unknown_and_written_back_unchanged` |
| A conflict mail goes to no ended session, and `conflict_mail = false` turns it off (DL-145). | `store` `apply_events` | `an_ended_session_gets_no_mail_and_the_live_one_still_does`; `conflict_mail_false_mails_nobody` |

## 8. Report problems

Note the time, the station, the session and what you did. Attach
`fleetdeck deck audit` output and the terminal log. Never paste an
invitation, a login link, a token or a password into a report.

More detail: the [operations guide](./operations.md).
