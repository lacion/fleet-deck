# M3 lane D (ops) log

LANE COMPLETE (2026-10-12): 7-5 Packaging, 7-4 Performance and 6-11 Human shell are done. The full `make check` belongs to lane A.

Lane D owns 7-5 Packaging, 7-4 Performance and 6-11 Human shell, in this order (M3 parallel lanes, 2026-10-12). Worktree `fleet-deck-worktrees/m3-ops`, branch `m3/ops`. Decisions: [decisions/m3-ops.md](../decisions/m3-ops.md) (DL-400 to DL-499).

## Items

- [x] 7-5 Packaging (round 1, DL-400)
- [x] 7-4 Performance (round 1, DL-401; defect DL-402)
- [x] 6-11 Human shell (round 1, DL-404; review fixes round 2, DL-405)

## Round log

### Lane D round 1 (2026-10-12)

- Start: HEAD `106c5b5`, clean tree; `git merge --no-edit v2` took `af8c78b` (schema V14 `shims.adapter_gap`, DL-124). Nothing to commit from an earlier round. `web/node_modules` exists.
- 7-5 Packaging (DL-400):
  - `dist-workspace.toml` (cargo-dist: Linux and macOS targets, shell and Homebrew installers, build setup for `web/dist`, packaging files in every archive); `.github/build-setup.yml`; `[package.metadata.dist] dist = true` in `crates/fleetdeck-cli/Cargo.toml`.
  - `packaging/systemd/fleetdeck-{station,deck}.service` (user units; the station unit has `KillMode=process`, so shims survive a restart or an upgrade), `packaging/launchd/dev.fleetdeck.{station,deck}.plist` (station: `AbandonProcessGroup`), `packaging/container/Containerfile` (three stages, non-root, `tini`, `/data` volume), `.dockerignore`, `packaging/README.md` (install, logs, owner steps).
  - Tests `crates/fleetdeck-cli/tests/packaging.rs`: `the_cargo_dist_config_releases_fleetdeck_for_linux_and_macos_with_the_packaging_files`, `the_systemd_units_start_real_commands_and_the_station_unit_keeps_its_agents`, `the_launchd_agents_are_well_formed_and_start_real_commands`, `the_container_file_builds_a_non_root_station_image_with_pinned_versions`, `the_offline_built_binary_reports_its_version`. `make package-check` added.
  - `cargo test -p fleetdeck-cli --test packaging` → 5 passed. Negative check: `fleetdeck station bogus --help` exits 2, `fleetdeck station serve --help` exits 0 (the unit check rejects a command that does not exist).
- 7-4 Performance (DL-401):
  - `crates/fleetdeck-cli/tests/perf.rs::performance_bars_of_plan_section_13` (`#[ignore]`), `make perf` (release build of the fakes, then the test with `--ignored`).
  - `make perf` → 1 passed, `target/perf/report.md`:

    Host: AMD Ryzen 9 9950X3D 16-Core, 32 threads, 43.6 GiB RAM; Ubuntu 24.04.5 LTS, Linux 6.6.87.2-microsoft-standard-WSL2. Profile release; loopback 127.0.0.1, TLS links, HTTPS board API (a new TLS connection per request); `fake-claude sim`; one deck and 10 stations as separate processes. Samples: read-model lag 77, approval round trip 20; run time 9 s.

    | Bar (PLAN §13) | Target | Measured | Result |
    | --- | --- | --- | --- |
    | Station overhead, idle (VmRSS) | < 30 MB | 17.8 MiB | pass |
    | Station overhead per session (VmRSS, 50 sessions) | < 5 MB | 0.09 MiB (22.2 MiB total) | pass |
    | Sessions per station | 50 live and idle | 50 started, 50 agent processes | pass |
    | Approval round trip board → agent (loopback, p95) | < 300 ms (LAN) | p50 22.1 ms, p95 26.8 ms, max 38.9 ms | pass |
    | Stations per deck | 10 linked, sessions on the board | 10 paired, 10 reachable | pass |
    | Deck read-model lag (p95) | < 500 ms | p50 34.5 ms, p95 48.4 ms, max 69.7 ms | pass |

  - A debug-profile run gives an idle station VmRSS of 34.5 MiB (fails the bar); the bar applies to the release binary that `make perf` measures.
  - Defect found (DL-402): the station keys runtime requests by `request_id` alone, but the adapters number them per session; the second session's request with the same ordinal is not stored and the board answer is `unknown_request`. Not fixed in lane D (station store, lane A code); the harness uses one session with board follow-ups.
- Checks: `cargo fmt --all --check` → ok; `cargo clippy --workspace --all-targets -- -D warnings` → ok (no warnings). web/ unchanged.
- 6-11 Human shell (DL-404):
  - Proto: `crates/fleetdeck-proto/src/shell.rs` (`ShellChange`, `ShellEnvelope`, `ShellInfo`, `ShellState`, `ShellAttach`, `ShellAttached`, limits), `Mutation::Shell`, feature `shell.v1`; TS types in `web/src/gen/shell/` (`make ts-types`). Link method rows `shell.list`, `shell.attach`, `shell.open`, `shell.resize`, `shell.close` (`crates/fleetdeck-link/src/methods.rs`, ADR-004 §9.6 rows and paragraph).
  - Station: `crates/fleetdeck-station/src/shell/{mod,scrollback,data,tests}.rs` (PTY via `portable-pty` 0.9, policy checks, cap, scrubbed environment, 5,000 lines / 8 MiB scrollback, data-channel stream service); link wiring in `link/{mod,intake,runtime}.rs` (`shell.list`, `shell.attach` stream tokens, receipts for the three mutations); `serve.rs` builds the shells from `allow_shell` and `max_shells` and ends them on stop.
  - Deck: `crates/fleetdeck-deck/src/link/streams.rs` (data channel per link), `http/shell_routes.rs` (list, audited commands, WebSocket relay), method rows `shells.list`, `shells.command`, `shells.attach` (admin).
  - Board: `web/src/shell/{shell.ts,ShellPanel.tsx}`, the panel on each station card for an admin, xterm.js 6 (`@xterm/xterm` 6.0.0, `@xterm/addon-fit` 0.11.0).
  - Tests: station `shell::tests::shell_policy_checks_allow_shell_the_roots_and_the_shell_cap`, `a_shell_runs_in_its_cwd_with_the_scrubbed_environment`, `a_reattached_board_gets_the_scrollback_and_the_shell_survives_a_detach`, `shell::scrollback::tests::the_scrollback_keeps_at_most_5000_lines_and_8_mib`, `a_byte_cut_keeps_the_line_count_right` → 5 passed; proto `shell::tests::shell_changes_check_their_ids_and_terminal_sizes` and the envelope table test (20 methods) → `cargo test -p fleetdeck-proto` all passed; `cargo test -p fleetdeck-link --test method_table` → 2 passed; deck `tests/board_shell.rs`: `the_board_opens_a_station_shell_and_reattaches_to_its_scrollback_after_a_link_drop` (open, echo, link drop and reattach with the scrollback, resize, refusals, close, audit rows, a drive-scope token gets 403), `a_station_with_allow_shell_off_refuses_every_shell` → 2 passed; web `bun test src` → 107 passed (3 new in `src/shell/shell.test.ts`); Playwright at the phone viewport `e2e/shell.spec.ts` (open, refusal outside the roots, typing, detach and reattach with the scrollback, close; unreachable station; no admin scope) → 3 passed; `e2e/stations.spec.ts` → 3 passed.
- Checks: `cargo fmt --all --check` → ok; `cargo clippy --workspace --all-targets -- -D warnings` → ok; `bun run --cwd web lint` → ok; `bun run --cwd web typecheck` → ok.

### Lane D round 2 (2026-10-12)

- Start: committed round 1 as `3a7e777`; `git merge --no-edit v2` → `9365f52` (conflicts kept both sides: link method count 35, mutation sample count 21, `version::ALL` keeps `shell.v1` and `auto_continue.v1`); `make ts-types`; proto and method-table tests passed.
- Review of 6-11 asked for three fixes (the review blocks the lane, so they are done now, DL-405):
  - Durable shell effects: `crates/fleetdeck-station/src/store/shell_effects.rs` (schema V15 `shell_effects`, enqueue, claim, finish, start recovery), `crates/fleetdeck-station/src/shell/mod.rs` (`check` without I/O, `decide` enqueues in the receipt transaction, `dispatch` does the PTY I/O after the commit), `link/intake.rs` (decide, then dispatch), `store/recovery.rs` (start recovery retires shell effects).
  - Bounded reader queues: `SUBSCRIBER_QUEUE = 256`; a full queue ends that reader only.
  - Login end: `crates/fleetdeck-deck/src/http/shell_routes.rs` relay rechecks `principal_alive` every `ws_recheck` and closes with 4401.
- Tests: `cargo test -p fleetdeck-station --lib shell::` → 8 passed (3 new); `cargo test -p fleetdeck-deck --test board_shell` → 3 passed (1 new).
- Checks: `cargo fmt --all --check` → ok; `cargo clippy --workspace --all-targets -- -D warnings` → ok. web/ unchanged.

## Review follow-ups (after M3)

- DL-402 (7-4): key `runtime_requests` by `(session_id, request_id)` or make request ids unique per station; regression test with two sessions that ask at the same ordinal. Files: `crates/fleetdeck-station/src/store/{schema,requests}.rs`, `crates/fleetdeck-station/src/manager/decide.rs`.
- 7-4: measure the LAN approval round trip on two hosts (owner step); add the deck VmRSS with 10 stations to the report.
- 7-5: run `dist generate` and commit the release workflow; build the image once and record its size (owner steps).
- 6-11: a shell that exits stays in the list until a close; add an automatic removal after a grace time. Files: `crates/fleetdeck-station/src/shell/mod.rs`.
- 6-11: the board resizes the PTY with one audited command per fit; debounce the resize. Files: `web/src/shell/ShellPanel.tsx`.
- 6-11: a station-restart test through the real `fleetdeck station serve` process (the shell ends, the list is empty after the restart). Files: `crates/fleetdeck-cli/tests/`.
- 6-11: measure the keystroke round trip (board → PTY echo) in `make perf`. Files: `crates/fleetdeck-cli/tests/perf.rs`.
- 6-11 (round 2 review): the three findings (durable effects, bounded queues, socket end on logout) are fixed in round 2 (DL-405) because they blocked the lane result.
- 6-11: a `failed` or `outcome_unknown` shell effect is visible only in the station store; show it in the board answer and the shell list. Files: `crates/fleetdeck-station/src/shell/mod.rs`, `crates/fleetdeck-deck/src/http/shell_routes.rs`.
- 6-11: a logout ends an attached shell socket only at the next recheck (30 s); push the end at once from the logout and revoke routes. Files: `crates/fleetdeck-deck/src/http/{auth_routes,shell_routes}.rs`.
