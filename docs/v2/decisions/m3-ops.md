# M3 lane D (ops) decisions: DL-400 to DL-499

Lane D (ops) records its decisions here (M3 parallel lanes, 2026-10-12). Each entry has the context, the options, the choice, the reason and the owner review state.

## DL-400: packaging (M3 item 7-5)

- Context: PLAN §12 and Phase 7 ask for `cargo-dist` (shell installer, Homebrew, GitHub releases), systemd and launchd units and a station container image. The build machine has no `dist` tool, no container runtime, no systemd user session for tests and no macOS host.
- Options for the release config: (a) write `dist-workspace.toml` by hand and let the owner run `dist generate`; (b) install `dist` and generate the release workflow now. Options for the units: (c) system units in `/etc/systemd/system` with a dedicated user; (d) user units (`systemctl --user`, `~/Library/LaunchAgents`) for the owner's OS user.
- Choice: (a) and (d).
  - `dist-workspace.toml` (cargo-dist 0.30 schema): targets `{x86_64,aarch64}-unknown-linux-gnu` and `{x86_64,aarch64}-apple-darwin` (A11); installers `shell` and `homebrew` (tap `lacion/homebrew-tap`); `install-path = "CARGO_HOME"`; `github-build-setup = "../build-setup.yml"` builds `web/dist` before `cargo build` (the deck embeds the board); the units, the container file and `packaging/README.md` go into every archive. `fleetdeck-cli` opts in with `[package.metadata.dist] dist = true` (the workspace has `publish = false`).
  - The station units keep the agents through a stop, a restart and an upgrade: systemd `KillMode=process` (the default control-group mode kills every shim of the unit), launchd `AbandonProcessGroup`. This does not reopen the ADR-005 §8 alternative ("KillMode=process instead of shims"): the shims still own the output and the fencing; the unit setting only stops systemd from killing them (ADR-005 §7 "Station graceful stop").
  - The container file `packaging/container/Containerfile` has three stages (bun 1.4.2 board build, Rust 1.96 build with `--locked`, Debian bookworm-slim runtime), a non-root user `fleetdeck` (UID 10001), `tini` as PID 1 (it reaps ended shims), the data volume `/data` (`FLEETDECK_DATA_DIR`), no provider CLI (a derived image adds it). A container stop ends every agent (like a reboot).
- Reason: (a) the generated workflow must come from a `dist` run that the owner can repeat; a hand-written workflow would drift. (d) the station runs the provider CLIs with the owner's login (PLAN §6.8); a system unit needs a second login per user.
- Evidence: `crates/fleetdeck-cli/tests/packaging.rs` (5 tests; `make package-check`; also part of the workspace tests of `make check`): every command that a unit or the image starts is run against the built binary with `--help`, so a renamed command fails the check.
- Owner steps: `dist init`/`dist generate` with network access (writes `.github/workflows/release.yml`), the tap repository, the image build (`docker build -f packaging/container/Containerfile .`), launchd on a macOS host.
- Owner review: pending.

## DL-401: performance harness (M3 item 7-4)

- Context: PLAN §13 bars: approval round trip board→agent < 300 ms on a LAN; deck read-model lag p95 < 500 ms; 50 sessions per station; 10 stations per deck; station overhead < 30 MB + < 5 MB per session (agent process excluded). The build machine has one host (WSL2) and no LAN peer.
- Options: (a) a separate benchmark binary; (b) an `#[ignore]` integration test on the existing real-process harness (`crates/fleetdeck-cli/tests/common`), run by `make perf` in the release profile.
- Choice: (b), `crates/fleetdeck-cli/tests/perf.rs::performance_bars_of_plan_section_13`. One deck and ten stations as separate processes on loopback (TLS links, separate data directories), `fake-claude sim` agents. Method: station memory is `VmRSS` of the station process (agents are other processes) idle and with 50 live idle sessions; the read-model lag is the time from the receipt of `station run` to the card in `GET /api/sessions` (2 ms polling, a new TLS connection per poll, so it is an upper bound); the approval round trip is the time from `POST /api/commands/approval.answer` (new TLS connection) to the answer line on the agent's stdin (1 ms polling). The test writes `target/perf/report.md` and fails when a bar fails.
- Reason: the harness reuses the tested process control, so it stays reproducible with one command; an ignored test keeps `make check` fast and offline.
- Limits: loopback is not a LAN; the LAN approval round trip is an owner step (two hosts). The report records the host, the profile, the transport and the sample counts.
- Owner review: pending.

## DL-402: request ids collide between sessions of one station (defect found by 7-4)

- Context: the perf harness answered approvals on fresh sessions from the board and got `unknown_request` for the second session. Cause: the Claude adapter numbers request ids per session (`core.ids.next("req")` in `crates/fleetdeck-providers/src/claude/requests.rs:87`, also `req-{n}` in `codex/requests.rs:194`), but the station table `runtime_requests` has `request_id TEXT PRIMARY KEY` (`crates/fleetdeck-station/src/store/schema.rs:128`) with `INSERT ... ON CONFLICT (request_id) DO NOTHING` (`store/requests.rs:264-268`), and the answer lookup is by `request_id` alone (`store/requests.rs:173`, `manager/decide.rs:101`). When two sessions of one station reach the same ordinal, the second request is not stored, and the board cannot answer it.
- Options: (a) fix it in lane D now; (b) record it, work around it in the harness, and give it to the lane that owns the station store.
- Choice: (b). The fix changes the station store key (a schema version and every request query) or the adapter id format (replay fixtures and snapshots); both are outside lane D and touch the durable code of lane A. The harness measures every approval on one session with board follow-ups, which has unique ids.
- Reason: lane rules (do only lane items; a fix must not break another lane's work). The defect is real for any owner with two sessions that ask for approval at the same time.
- Follow-up: key `runtime_requests` by `(session_id, request_id)` (or make request ids unique per station) with a regression test "two sessions with the same native request ordinal can each be answered".
- Owner review: pending.

## DL-404: human shell (M3 item 6-11)

- Context: PLAN §6.8 and D10: the station owns a PTY with the owner's login shell; policy checks (`allow_shell`, `max_shells`, allowed roots); a scrubbed environment; open, resize and close are audited commands with a `command_id`; keystrokes and output go only on the link data channel (A7) and are not audited; at most 5,000 lines and 8 MiB of scrollback; a reconnecting board gets the scrollback, then the live bytes; the shell survives a deck, board or network failure; a station restart ends it (A10). The plan does not name the link methods, the board API or the board scope.
- Choice:
  - PTY: `portable-pty` 0.9 (the crate of PLAN §12 for PTYs). Station module `crates/fleetdeck-station/src/shell/` (`Shells` registry, reader thread per shell, `Scrollback` ring with both limits). The environment is `ChildEnv` (the same scrub as agents) plus `TERM=xterm-256color`. The program is `$SHELL -l`, else `/bin/sh -l`.
  - Link (ADR-004 extension rule, feature `shell.v1`): `shell.list` and `shell.attach` (non-mutating; `shell.attach` returns a single-use `stream_token` for a data stream with the purpose `shell:<shell_id>`), and `shell.open`, `shell.resize`, `shell.close` as mutating methods with the `ShellEnvelope` (command id, target incarnation, retry window, `ShellChange`). They go through the station receipt table like every mutation, so a retry with the same command id gets the same receipt. Rejects: `unsupported` (`allow_shell = false`), `invalid_command` (a bad field, a cwd outside the allowed roots), `invalid_state` (`max_shells`, a used id, an unknown shell).
  - Deck: the deck keeps the data channel of each live link (`link/streams.rs`, `DataHub`); board API `GET`/`POST /api/stations/{station}/shells` and the WebSocket `GET /api/stations/{station}/shells/{shell}/attach` (binary frames both ways). The deck writes one audit row for each open, resize and close (accepted or refused, with the command id); it writes no row for keystrokes.
  - Scope: the board shell routes need the `admin` scope (the shell is owner-only and can do everything that the owner can do on the station host), although the link methods use the `drive` scope (the link has no admin role for a deck; the station keeps its own `allow_shell` gate).
  - Board: a "Shells" panel on each station card of the stations screen (admin only) with an xterm.js 6 terminal (`@xterm/xterm` 6.0.0, `@xterm/addon-fit` 0.11.0, loaded on demand). Detach keeps the shell; Attach shows the scrollback again.
  - Lifetime: shells belong to the station process, not to a link: a link drop or a deck restart does not end them; `RunningStation::stop` and a process exit end them.
- Reason: the smallest design that meets §6.8 with the existing receipt, audit and data-channel machinery; no new table and no new schema version.
- Limits (follow-ups): the deck holds no shell state, so the scrollback lives only on the station; a shell that exits stays in the list (state `exited` and the exit code) until a close; the board terminal resizes the PTY with an audited command on each fit.
- Owner review: pending.

## DL-405: shell commands are durable effects; bounded shell queues; shell sockets end with the login (review of 6-11)

- Context: the review of 6-11 (round 2) found three defects. (1) `shell.open`, `shell.resize` and `shell.close` did the PTY I/O inside the receipt transaction (`crates/fleetdeck-station/src/link/intake.rs`), so a crash could commit a receipt for I/O that never happened, or do I/O for a receipt that rolled back; ADR-001 wants side effects as durable effects after the commit. (2) Each attached reader had an unbounded queue, so a slow board could grow the station memory without a limit. (3) An attached shell WebSocket stayed open after a logout, a token revocation or an expiry.
- Options for (1): (a) a new `shell_effects` table with the same lifecycle as the session effects (queued, claimed, then confirmed or failed; start recovery retires queued rows and marks claimed rows `outcome_unknown`); (b) reuse the session `effects` table (it is keyed to sessions and provider incarnations, and its worker targets provider processes); (c) do the I/O first and record the receipt after it (a crash between the two gives I/O without a receipt).
- Choice: (a). The receipt transaction only checks the change (pending opens and closes count for the id, the cap and unknown shells) and inserts one `shell_effects` row (schema V15, the end of the station migration list). After the commit, `shell::dispatch` claims the rows in order and does the PTY I/O on a blocking thread; the result goes to the row (`confirmed`, or `failed` with the reason). The station answers the link request after the dispatch, so the board sees the shell in the list at once. At station start, `recover` sets queued rows to `retired_not_dispatched` and claimed rows to `outcome_unknown` (detail `station_restart`); nothing runs after a restart, which matches A10 (a restart ends every shell). A retry with the same command id gets the stored receipt and runs nothing.
- Choice for (2): each reader gets a bounded queue of 256 chunks (`SUBSCRIBER_QUEUE`). A full queue ends that reader (its data stream closes); the shell keeps running and the scrollback keeps everything within 5,000 lines and 8 MiB, so the board can attach again.
- Choice for (3): the relay checks `principal_alive` every `ws_recheck` (30 s in production, as the board WebSocket), and closes the socket with code 4401 ("login ended") and closes the data stream. A database error counts as an ended login (fail closed). The shell keeps running.
- Reason: the same durable-effect rule as every other station side effect; bounded memory per reader; the same login-end rule as the board WebSocket.
- Merge note: lane A also adds a station schema V15; renumber at the merge (lane rule).
- Tests: `shell::effects_tests::{a_shell_command_committed_before_a_crash_never_runs_after_the_restart, shell_effects_run_after_the_commit_in_order_and_a_pty_failure_is_recorded, a_slow_reader_is_cut_off_and_the_shell_keeps_running_with_its_scrollback}` (station lib), `an_attached_shell_socket_closes_after_a_logout_and_a_token_revocation` (`crates/fleetdeck-deck/tests/board_shell.rs`).
- Supersedes the "no new table and no new schema version" line of DL-404.
- Owner review: pending.

## DL-403: owner decision, fast rounds and parallel lanes (approved)

- The owner (2026-10-12) asked for a usable M3 soon: each item gets one review, findings go to the "Review follow-ups (after M3)" list of the lane log, normal rounds run only the format and lint checks, and the full `make check` runs once at the end in lane A. Four lanes build M3 in parallel; lane D owns 7-5 Packaging, 7-4 Performance and 6-11 Human shell, in this order.
- Owner review: approved (owner decision).
