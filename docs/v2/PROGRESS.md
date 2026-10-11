# Fleet Deck v2 — progress (goal loop memory)

Current milestone: **M2 = PLAN Phases 1–5** (M1 = Phase 0 is complete, see the M1 section below; M3 = Phases 6–7). The M2 goal loop has at most 50 rounds for M2 and M3 together.

Rules for this file: one checklist item per deliverable, in PLAN order. Each round updates the item status, the commands it ran, and the results. The work of a round stays uncommitted for the reviewer; the next round commits it first.

## M2 checklist

Sources: PLAN §14 Phases 1–5, GATE-M1 G1–G14, goal acceptance. G13 is P1-5. G14 and G1–G7 (contract) come before any Phase 3 link code (item G-C). G8–G12 are implementation items inside Phase 3; each one names its test. The **M2 acceptance** column says which exit check of the goal an item proves; the test names are filled in when the item is done.

### Phase 1 — Foundations

- [x] **P1-1 Workspace and skeletons.** Crates `crates/fleetdeck-{proto,core,link,providers,station,deck,cli,testkit}` (PLAN §12) as workspace members beside the spikes; the `fleetdeck` binary with every subcommand of PLAN §1, §6.5, §8.5 and G12 (stubs that fail with "not implemented" until their phase); CLI parse tests (G12: no invitation argument). *(M2 round 1: tests `fleetdeck-cli cli::tests::{the_command_tree_is_consistent, every_plan_subcommand_parses, an_invitation_given_as_an_argument_is_refused, stdin_and_invite_file_exclude_each_other}`)*
- [x] **P1-2 `fleetdeck-proto` v1.** Canonical events (§6.3, with the L6/L7 ids and `provider_refs`), capabilities (§6.2 groups + degradations), commands and receipts (§6.4), link frames (§8.3 + G4 `incarnation` and snapshot batches), protocol version + feature negotiation; `ts-rs` export; serde round-trip and proptest decode tests; old-event decode rule (L16). *(M2 round 1: modules `ids`, `version`, `capability`, `event` (+`event_tests`), `command`, `session`, `link`, `ts`; 22 unit tests incl. `event::tests::{every_kind_has_a_sample_and_round_trips, an_unknown_kind_is_kept_verbatim, a_known_kind_is_strict_about_required_fields}`, `version::tests::negotiation_is_symmetric` (proptest), `command::tests::admission_checks_the_incarnation_and_the_deadline`, decode proptests `event::tests::decode_props::{event_decode_is_total, control_frame_decode_never_panics}`; drift test `fleetdeck-proto::ts_drift` (2 tests); `make ts-types`; vocabulary deviations in DL-027)*
- [x] **P1-3 `fleetdeck-core`.** Ids (ULID), `Secret<T>` (no `Serialize`, redacted `Debug`), identity keys, config (`station.toml` with `[[provider_instance]]` L10, deck config), SQLite on a dedicated DB thread + `rusqlite_migration`, logging (`tracing`, JSON option), injected `Clock` and id source. *(M2 round 2: modules `clock`, `ids`, `secret`, `identity`, `netscope`, `paths`, `config` (+`config_tests`), `db` (+`db_tests`), `logging`; 51 unit tests incl. `secret::tests::{debug_output_is_redacted, secrets_cannot_be_serialized}`, `ids::tests::{ulids_are_strictly_increasing_inside_one_millisecond, sequential_ids_are_deterministic_and_typed}`, `identity::tests::{save_and_load_keep_the_identity_and_protect_the_file, a_symlinked_key_file_is_refused, concurrent_creation_ends_with_one_key}`, `config::tests::{the_plan_example_parses, two_accounts_on_one_driver_are_two_instances, unknown_keys_and_bad_values_are_errors, listen_addresses_follow_the_d13_bind_rule, files_load_with_owner_rules}`, `db::tests::{a_file_database_uses_wal_full_sync_and_foreign_keys, migrations_upgrade_an_existing_database_and_keep_its_data, a_newer_database_is_refused, a_failing_migration_rolls_back_and_reports, async_calls_from_many_tasks_are_serialized, errors_and_panics_in_a_job_do_not_stop_the_thread}`, `logging::tests::json_lines_parse_and_redact_secrets`; CLI test `cli::tests::the_log_format_is_text_or_json`; DL-028)*
- [x] **P1-4 `fleetdeck-testkit` skeleton and `web/` skeleton.** Testkit: fake clock, id source, temp data dirs, fake-agent harness entry points. `web/`: React + TS + Vite (bun), ported design tokens from `board/src`, imports the generated types from `web/src/gen/` (written by `make ts-types` in round 1); `bun run typecheck|lint|test`. *(M2 round 3: testkit modules `det`, `node`, `fake` + binaries `fake-claude`, `fake-codex`; 14 tests incl. `det::tests::two_runs_give_the_same_ids_and_times`, `node::tests::{nodes_get_private_separate_directories, the_test_station_config_loads}`, `fleetdeck-testkit::fake_bins::{locate_finds_the_binaries_that_cargo_built, version_lines_look_like_the_real_clis, exit_scenarios_set_the_exit_code, hang_waits_for_stdin_to_close}`; `web/` tests `src/board/lanes.test.ts` (3) and `src/App.test.tsx` (2), the fixture `src/testing/summary.ts` is a full generated `SessionSummary` literal, so `bun run typecheck` fails when the Rust type changes; DL-029)*
- [x] **P1-5 CI and `make check` (G13).** TS-types drift check (fails on a stale file, with a test that proves it); `make check` runs fmt, clippy, nextest, deny, the web checks, the drift check, and every replay; a v2 CI workflow file. Exit: CI green on the wired workspace; generated TS types imported by `web/`. *(M2 round 4: `make check` = `tools/check.sh`, 12 commands (fmt, clippy, nextest, deny, `cargo test -p fleetdeck-proto --test ts_drift`, offline `bun install`, web typecheck/lint/test/build, `replay-claude`, `replay-codex`); drift tests `fleetdeck-proto::ts_drift::{committed_ts_bindings_match_the_rust_types, the_drift_check_detects_stale_missing_and_extra_files}`; web import proof `web/src/board/lanes.ts` + `web/src/testing/summary.ts` (typed from `web/src/gen`); CI file `.github/workflows/v2.yml` runs the same `make check`; evidence for "CI green": fresh `git clone` + `unshare -rn` (no network) → all 12 passed; a stale `web/src/gen/session/SessionSummary.ts` → `make check` exit 2 with "stale: session/SessionSummary.ts"; DL-030)*

### Phase 2 — Station core

- [ ] **P2-1 `SessionHost` + `DirectChildHost`.** Process groups, clean stop, kill, exit reporting; env scrub for children (§3 rule 6).
- [ ] **P2-2 Adapter traits + Claude core.** `ProviderAdapter`, `LiveSession`, `Capabilities`, typed `Unsupported { op, degradation }` (L5); the Claude sans-IO core copied from `spikes/s1-claude` and hardened, mapped to `fleetdeck-proto` events; traps 1–10 (t3code-lessons §5).
- [ ] **P2-3 Claude driver, raw-frame log, fake-claude, replays.** Live driver on `SessionHost`; bounded rotating raw-frame log (L12); `fake-claude` binary in the testkit; all Claude fixtures replay through the production adapter.
- [ ] **P2-4 Station event log and receipts.** `station.db` migrations (§10); `station_seq`, `session_seq`, `incarnation_id`, retention; command receipts (duplicate → stored receipt; reused id with another method/payload/caller → rejected).
- [ ] **P2-5 Effects and recovery.** Effect outbox (`queued → claimed → confirmed | retired_not_dispatched | outcome_unknown`), claim with fence token + provider incarnation before I/O, effect worker, startup recovery before admission (A10, L2, L3), runtime requests table, offline approval policy (§6.5).
- [ ] **P2-6 Session manager and policy.** Commands spawn/prompt/steer/follow-up/interrupt/answer/set-mode/stop through decide → commit → effect → adapter; `station.toml` policy (allowed roots, max sessions, provider instances).
- [ ] **P2-7 Station-local CLI.** `fleetdeck station sessions|run|approve|interrupt` over a Unix socket. Exit test: a Claude session (fake-claude) runs, asks for approval, is answered and interrupted from the CLI.
- [ ] **P2-8 Fault tests (§6.4, §13).** Crash before commit, after commit, after the dispatch claim, after the send, before the confirmation is stored; worker restart and provider restart separately; stale approval reply; duplicate replays.

### Gate contract (before Phase 3 code)

- [ ] **G-C G14 + G1–G7 contract.** ADR-004 "Revision 2 (M1 gate)", PLAN §8/§9 updates, DECISIONS-LOG entry pointing to GATE-M1.

### Phase 3 — Deck core + link

- [ ] **P3-1 Link base.** `fleetdeck-link` from `spikes/s4-link` (addr rules, identity, TLS, transport, hello/handshake); node-wide link table, persistent per-peer epoch + `attempt_id`, reciprocal-dial rule (G1); generation-aware cleanup (G10).
- [ ] **P3-2 Control channel.** Role/direction checks and feature gates for every frame (G6); method table with roles, scopes, mutating flag (G7, table-agreement test); bounded queues, heartbeat 15 s / 45 s, 128 outstanding requests, shutdown never blocks (G8).
- [ ] **P3-3 Data channel.** Budgets (64 streams, 256 KiB window, 4 MiB buffered), empty frame = error, single-use stream tokens (G2, G6); reset fails pending writes, no id reuse (G9).
- [ ] **P3-4 Pairing, revocation, reconnect.** Per-invitation failure counts and rate limits (G11); revocation and key rotation close links within 1 s (G5); invitation input by prompt/stdin/`--invite-file` 0600 and log redaction (G12); jittered reconnect.
- [ ] **P3-5 Station link service.** Event stream with `incarnation`, acks, snapshot batches (G4); command intake with `target_incarnation` / `retry_until` (G3); receipts kept ≥ 24 h.
- [ ] **P3-6 Deck registry and read model.** `deck.db` migrations; station registry; read model apply with cursor, ack, snapshot batch, incarnation change; status projection (§7.2 incl. `unreachable`, `lost`).
- [ ] **P3-7 Command router and outbox.** `pending/done/failed/ambiguous` with the G3 rules; outbox (A5); coordinator command dedupe (G7).
- [ ] **P3-8 Auth and audit.** Owner login (argon2), session cookie, CSRF, WS `Origin` check, login also on loopback (A13), `fleetdeck open` one-time login URL, per-method scopes (L15), audit log.
- [ ] **P3-9 Board API.** REST commands + WS snapshot/deltas, per-session subscriptions with a cursor.
- [ ] **P3-10 Phase 3 exit tests.** Deck + 2 stations as separate processes on loopback (one dial-only, one dialed); link drops and deck restarts lose no events and duplicate no commands; public addresses rejected; scopes enforced.

### Phase 4 — Board MVP

- [ ] **P4-1 Connection runtime + login.** `web/src/runtime/` (L17): one retry owner, transport health vs freshness, stale cache, per-session cursors; login page.
- [ ] **P4-2 Fleet lanes, stations panel, pairing dialog.**
- [ ] **P4-3 Session view and needs-you rail.** Timeline, composer (prompt/steer/follow-up), interrupt, mode ladder, approvals.
- [ ] **P4-4 Spawn form and `fleetdeck up`.** Station + provider + directory picker; all-in-one mode with the embedded board (`rust-embed`).
- [ ] **P4-5 MVP demo test.** Playwright at a phone viewport against fake agents: spawn on two stations, answer an approval, steer, interrupt, survive a deck restart and a network drop.

### Phase 5 — Codex

- [ ] **P5-1 Codex core.** Sans-IO core from `spikes/s2-codex` (vendored schema) mapped to proto events; replays of all Codex fixtures.
- [ ] **P5-2 Codex driver and fake-codex.** Background polling (DL-024), station integration.
- [ ] **P5-3 Capability matrix tests + capability-driven UI.** Every §6.2 row covered for both providers; controls only when capabilities allow them.
- [ ] **P5-4 Codex board flows.** The Playwright flows for Claude and Codex.
- [ ] **P5-5 Live smoke tests.** One per provider (real CLI, tiny prompt, temp dir) behind `FD_LIVE_SMOKE=1`, documented; not in the default run.
- [ ] **M2-F Close-out.** Every exit check named in this file; `make check` on a clean checkout; DECISIONS-LOG complete.

## M2 round log

### M2 round 1 (2026-10-11)

- Start: working tree clean at `4888dbc`; nothing to commit.
- Added this M2 checklist (Phases 1–5, the GATE-M1 items, close-out).
- **P1-1 done.** Eight product crates under `crates/`; root `Cargo.toml` lists them before the spikes; workspace `clap` gets the `env` feature. `fleetdeck` binary (`crates/fleetdeck-cli`): full command tree (`up`, `open`, `deck serve|invite|add-station|stations|remove-station`, `station serve|invite|join|forget-deck|sessions|run|approve|interrupt`); `add-station`/`join` take the invitation only from a prompt, `--stdin` or `--invite-file` (G12). Each stub exits 1 with "`fleetdeck <cmd>` is not implemented yet (PLAN Phase N)".
- **P1-2 done.** `fleetdeck-proto` v1 (2,300 lines, every module < 800): ids, version negotiation (symmetric, required `control.v1`), capabilities with `Unsupported { op, degradation }` per §6.2 row, canonical events with the L16 unknown-kind rule, commands/envelopes/receipts with the G3 admission rules, session projection shape, control frames with G4 `incarnation` and snapshot batches, ts-rs export. Workspace deps: `ts-rs` 12 (`serde-json-impl`), `ulid` 1. `web/src/gen/` holds the 60 generated files + `index.ts`; `make ts-types` regenerates them. DL-027 records the vocabulary additions to PLAN §6.3.
- Commands and results:
  - `cargo nextest run -p fleetdeck-cli` → 4 passed; `cargo nextest run -p fleetdeck-proto` → 26 passed (first run: the symmetry proptest found that `InvalidRange` depended on the argument order; fixed by checking the ranges in sorted order).
  - `cargo run -q -p fleetdeck-cli -- station join` → "`fleetdeck station join` is not implemented yet (PLAN Phase 3)", exit 1.
  - `make check` → all 6 commands passed (fmt, clippy `-D warnings`, nextest 270 passed, deny, 29 Claude replays, 18 Codex replays).
  - `web/` has no `package.json` yet, so `bun run typecheck|lint|test` do not exist (P1-4). The drift check runs inside nextest (`fleetdeck-proto::ts_drift`).
- Next: P1-3 `fleetdeck-core`.

### M2 round 2 (2026-10-11)

- Start: review feedback "continue: P1-3 fleetdeck-core". Committed the round-1 work as `2d80eba` (`feat(v2): workspace skeletons, fleetdeck CLI tree and fleetdeck-proto v1 (M2 P1-1, P1-2)`).
- **Fix:** PLAN §11 names `web/src/gen/` for the generated types; round 1 used `web/src/generated/`. Moved the files (`git mv`) and changed the Makefile `ts-types` target, the drift test, `export_ts` and this file (the round-1 log above now also says `gen`).
- **P1-3 done.** `crates/fleetdeck-core` (2,700 lines with tests, largest module `config.rs` 511 lines):
  - `clock`: `Clock` trait (Unix ms), `SystemClock`, `ManualClock`. `ids`: `IdSource` trait, `UlidSource` (strictly monotonic, also inside one ms and when the clock goes back), `SeqIds` (deterministic, for replays), `new_id::<SessionId>()`.
  - `secret`: `Secret<T>` (redacted `Debug`, no `Serialize`/`Clone`/`Display`, `ct_eq`); a test proves that `Secret<String>` has no `Serialize` impl.
  - `identity`: copied from spike S4 and hardened: the key file is written to a temporary file and hard-linked into place (no partial key, no overwrite, concurrent `load_or_create` ends with one key), a symlinked or group/world-readable key file is refused, the directory is made 0700.
  - `netscope`: the D13 address rules of S4 (bind, dial, peer, resolved names), shared by config and the later link crate.
  - `paths`: `Env`, `NodeDirs` (XDG defaults, `--data-dir`, `deck/` and `station/` node dirs), `expand_path` (`~`, no `..`), `ensure_private_dir`.
  - `config`: `StationConfig` (PLAN §6.7 example parses verbatim; L10 provider instances; `env` as `Secret`), `DeckConfig`, `OfflineApprovalPolicy`.
  - `db`: `Db` with one DB thread, `call` (async) and `call_blocking`, WAL + `synchronous=FULL` + foreign keys, migrations with `rusqlite_migration`, refusal of a newer schema, panics in a job become `DbError::Panicked` and roll back.
  - `logging`: `LogFormat` text/json, filter from `FLEETDECK_LOG`/`RUST_LOG`; the `fleetdeck` binary has `--log-format` (env `FLEETDECK_LOG_FORMAT`), initialises logging and resolves the directories before each command.
  - DL-028 records the support crates (`toml`, `rustls-pki-types`, `rustls-webpki`, `ring`, `rustix`, `rusqlite` 0.40 + `rusqlite_migration` 2.6), the directory layout, `deck.toml` keys and the SQLite settings.
- Commands and results:
  - `cargo nextest run -p fleetdeck-core` → 51 passed (first runs: `usize` is not `FromSql` → `i64`; static `Migrations` cannot be built in a `static` → `Db::open` takes `Migrations<'static>` by value; text logs had ANSI colours → `ansi` only for a terminal).
  - `cargo nextest run -p fleetdeck-cli` → 5 passed. `FLEETDECK_LOG=debug fleetdeck --data-dir /tmp/fdx station sessions --log-format json` → one JSON debug line with the directories, then the "not implemented yet (PLAN Phase 2)" error, exit 1; no directory created.
  - `make check` → all 6 commands passed (fmt, clippy `-D warnings`, nextest 322 passed, deny, 29 Claude replays, 18 Codex replays).
  - `web/` checks: not yet (P1-4).
- Next: P1-4 `fleetdeck-testkit` skeleton and `web/` skeleton.

### M2 round 3 (2026-10-11)

- Start: review feedback "continue: P1-4 fleetdeck-testkit and web skeletons". Committed the round-2 work as `e16edf1` (`feat(core): add fleetdeck-core foundations (clock, ids, secret, identity, config, db, logging)`).
- **P1-4 done.**
  - `crates/fleetdeck-testkit`: `det::Det` (one `ManualClock` + `SeqIds` time line, start 2026-01-01, clones share it, typed `next_id::<SessionId>()`); `node::TempNode` (temporary fake home + `--data-dir`, mode 0700, `workspace()`, `station_toml()` with one provider instance per fake agent, `write_station_toml()` mode 0600, `station_config()` loads it like the station does); `fake::FakeAgent` (`locate()` via `FLEETDECK_FAKE_BIN_DIR` or the Cargo profile directory, `command(scenario)` sets `FLEETDECK_FAKE_SCENARIO`) and `fake_main`; binaries `fake-claude` / `fake-codex` answer `--version` like the real CLIs (`2.1.999 (Claude Code)`, `codex-cli 0.160.999`) and run the generic scenarios `exit:<code>` and `hang`. Protocol scenarios come with P2-3 (Claude) and Phase 5 (Codex).
  - `web/`: `package.json` (bun; React 19.2.8, Vite 8.2.1, TypeScript 5.9.3, Biome 2.5.8), `tsconfig.json` (browser, strict like the v0 board) + `tsconfig.test.json` (bun globals for tests), `vite.config.ts` (relative base, `dist/` for the later `rust-embed`), `biome.jsonc` (nested config, excludes `src/gen`), `index.html` (no external fonts), `src/styles/tokens.css` (copied from `board/src/tokens.css` + provider badge colours + 44 px touch target), `src/styles/app.css`, `src/board/lanes.ts` (lanes from the generated `Activity`; `satisfies Record<Activity, string>` makes the typecheck fail on a new variant), `src/board/FleetBoard.tsx`, `src/App.tsx`, `src/main.tsx`.
  - DL-029 records the web tool chain (`bun test` + Biome, because no cached Vitest supports Vite 8), the omitted React Compiler, no external fonts, and the fake-agent entry points.
- Commands and results:
  - `cargo nextest run -p fleetdeck-testkit` → 14 passed.
  - `cd web && bun install` → 32 packages, `bun.lock` written. `bun run typecheck` → ok. `bun run lint` → "Checked 10 files … No fixes applied" (first run: the copied `tokens.css` was not in Biome format → `bun run format`). `bun run test` → 5 pass, 0 fail. `bun run build` → `dist/` (192 kB JS).
  - `make check` → first run: `cargo fmt --all --check` failed on the new testkit files → `cargo fmt --all`; second run: all 6 commands passed (fmt, clippy `-D warnings`, nextest 336 passed, deny, 29 Claude replays, 18 Codex replays).
- Not yet: `make check` does not run the web checks; that is P1-5 (G13).
- Next: P1-5 CI and `make check` (G13).

### M2 round 4 (2026-10-11)

- Start: review feedback "continue: P1-5 CI and make check; include web typecheck, lint, tests, and TS drift verification in the offline acceptance command". Committed the round-3 work as `bde879e` (`feat(v2): add fleetdeck-testkit and web skeletons (M2 P1-4)`).
- **P1-5 done (GATE-M1 G13).**
  - `tools/check.sh` (new): the v2 acceptance script behind `make check`; 12 commands, one `ok`/`FAILED exit=N` line each with a short summary (nextest summary, replay counts, `bun test` pass/fail, Biome file count); exit 1 if any command fails. It seeds `target/advisory-dbs` from `$CARGO_HOME/advisory-dbs` on a fresh checkout and installs `web/` dependencies with `bun install --frozen-lockfile --offline`.
  - `Makefile`: `check` → `sh tools/check.sh`; new `check-spikes` (the M1 script `spikes/acceptance.sh`, unchanged) and `test-web`; `test` and `ts-types` unchanged.
  - `.github/workflows/v2.yml` (new): one job on push/PR to `v2`; pinned actions, Rust 1.96, pinned cargo-nextest 0.9.146 and cargo-deny 0.19.9, cache fill with network, then `make check`. The v0 `ci.yml` is unchanged. YAML parsed with `Bun.YAML.parse` (6 steps).
  - `crates/fleetdeck-proto/tests/ts_drift.rs`: reads `CARGO_MANIFEST_DIR` at run time first (found when a shared target dir made the worktree test read the clone's files).
  - DL-030 records the script, the offline rules and the CI evidence.
- Commands and results:
  - Fresh clone, no network: `git clone . $tmp/repo` + this round's files, `CARGO_TARGET_DIR=<worktree>/target unshare -rn sh -c 'ip link set lo up && make check'`. First run: s4-link socket tests failed because `unshare -n` leaves `lo` down (test setup, fixed with `ip link set lo up`), and `cargo deny check` failed with "cannot change to 'target/advisory-dbs/advisory-db-…'" (fresh checkout has no database; fixed by the seed step). Final run: `check: all 12 commands passed` (nextest 336 passed, ts_drift 2 passed, bun test 5 pass, Biome 10 files, 29 Claude + 18 Codex replays).
  - Negative proof: in the clone, `echo "// stale" >> web/src/gen/session/SessionSummary.ts`; `make check` → `FAILED … cargo test -q -p fleetdeck-proto --test ts_drift` with "web/src/gen is out of date; run `make ts-types`: stale: session/SessionSummary.ts", `check: 2 of 12 commands FAILED`, make exit 2.
  - Worktree: `make check` → `check: all 12 commands passed` (fmt ok, clippy `-D warnings` ok, nextest 336/336, deny ok, ts_drift 2/2, bun install ok, typecheck ok, lint ok, bun test 5 pass, build ok, replays 29 + 18). `make test-web` → ok (5 pass).
- Owner step: a real GitHub Actions run of `.github/workflows/v2.yml` (no push in this loop).
- Next: P2-1 `SessionHost` + `DirectChildHost` (G-C stays before any Phase 3 link code).

## M1 checklist (complete)

- [x] **P0-1 Scaffold.** Root Cargo workspace (spike members only, DL-001), `deny.toml`, lint policy (`unsafe_code = forbid`, `clippy::unwrap_used = deny` outside tests), `spike-canon` (canonical event subset of §6.3), `docs/v2/DECISIONS-LOG.md`, `spikes/README.md`, this file. *(round 1)*
- [x] **S1-1 Claude protocol core.** Frame parser (never panics, unknown frames kept, proptest), host control frames, sans-IO adapter core: init, prompt, partial deltas, `can_use_tool` with native option ids + edited input, idempotent answers, fail-closed unknown requests, interrupt, set mode (native modes only), exit expiry. *(round 1; round 2 review fix: a prompt is confirmed only by `command_lifecycle` `started` with the matching uuid, DL-006)*
- [x] **S1-2 Claude live driver + fixture format + `record-claude`.** tokio process driver around the core; JSONL fixture format (`spawn`/`host`/`in`/`out`/`raw`/`exit`/`note` records + header, scrubbed); `record-claude`; live recordings (haiku, `--safe-mode`, temp dir) of all 8 §13 shared scenarios plus `tool_call_edit_input`, `interrupt_during_tool`, `set_permission_mode`, `approval_restart`. *(round 2; an early replay harness + `replay-claude` is pulled forward from S1-4)*
- [x] **S1-3 TypeScript SDK reference recordings.** `spikes/s1-claude/sdk-recorder/` (bun + `@anthropic-ai/claude-agent-sdk` 0.3.296, `tee-claude.mjs` as `pathToClaudeCodeExecutable`); 10 SDK fixtures in `spikes/s1-claude/fixtures/sdk/` (8 shared + `tool_call_edit_input`, `set_permission_mode`); `reference` module + `replay-claude` replay them offline (DL-008). *(round 3)*
- [x] **S1-4 Claude replay + evidence.** *(round 2: `replay` module, `replay-claude`, `tests/replay_fixtures.rs`.)* *(round 3: replay rejects unmatched writes before `exit`; SDK reference replay.)* *(round 4: `insta` snapshots of the canonical event stream of all 22 fixtures; `tests/evidence.rs`; `spikes/s1-claude/EVIDENCE.md` for "prompt confirmed", queue/steer timing, restart survival, native option ids; SDK recorder exits nonzero and kills timed-out sessions + `selftest.sh`.)*
- [x] **S1-5 ADR-001** (Proposed): native Claude adapter design, effect states and crash boundaries (§6.4), fixture set, unsupported scenarios with reasons. *(round 5: `docs/v2/adr/ADR-001-native-claude-adapter.md`; 7 extra recordings for the matrix rows; `tests/capabilities.rs`; EVIDENCE.md E7; DL-010, DL-011)*
- [x] **S2-1 Codex protocol core.** ADR-002 option evaluation (git pin of `codex-app-server-protocol` vs codegen from `codex app-server generate-json-schema`); JSON-RPC framing; sans-IO core: initialize, `thread/start`, `turn/start`, both approval requests, questions (L4), `turn/interrupt`, `thread/resume`, diff notifications. *(round 6: `spikes/s2-codex` with `rpc` + `adapter` (mod/map/requests/host); option evaluation → DL-012 (vendored schema `schema/0.160.0/`, `tests/schema_methods.rs`); experimental API + mode names → DL-013; 23 tests)*
- [x] **S2-2 Codex live driver + `record-codex` + fixtures** for the §13 shared scenarios Codex supports (unsupported ones listed with reasons); restart survival experiment. *(round 7: 8 shared + 7 extra fixtures, none unsupported; `replay-codex` and evidence tests done early)*
- [x] **S2-3 Codex replay + `replay-codex` + ADR-002** (Proposed) with effect states and crash boundaries. *(round 7: replay + `replay-codex` + evidence tests; round 8: ADR-002, schema conformance, both question channels)*
- [x] **S4-1 Link identity + address rules.** Ed25519 identity keys, self-signed certs from them, mTLS with pinned-key verifiers, D13 address rules (loopback, RFC 1918, ULA, link-local, Tailscale ranges; reject public at bind and dial) as unit tests. *(round 9)*
- [x] **S4-2 Link handshake + channels.** `Hello` with `proto_min/max`, feature flags, roles, `link_epoch`, nonce; transcript signature bound to the TLS exporter; version negotiation; control channel (frames of §8.3) and data channel (multiplexed streams, credit flow control). *(round 10)*
- [x] **S4-3 Link pairing + reconnect.** `fdinv1:` invitations both directions (single use, 15 min, fingerprint-bound, secret proof bound to TLS); jittered backoff reconnect; duplicate link (`link_epoch`) rule; station-only-dials (NAT) case; multi-process loopback test with separate data dirs. *(round 11)*
- [x] **S4-4 ADR-004** (Proposed): link protocol v1 (frames, handshake, pairing, scopes, reconnect, feature flags). *(round 12; plus WebSocket upgrade and setup timeouts, DL-023)*
- [x] **M1-F Close-out.** PLAN §6.2 matrix: verified value + evidence per cell; `spikes/README.md` offline commands for every spike; full acceptance run; DECISIONS-LOG complete. *(round 13; plus the review fix for post-upgrade timeouts and the TCP self-connect guard)*

## Round log

### Round 1 (2026-10-11)

- Done: P0-1; S1-1 core (frames, control, adapter core + tests).
- Live probe (one run, haiku, temp dir): `claude -p --input-format stream-json --output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio --model haiku`. Findings:
  - `initialize` control request → `control_response` with `commands`, `agents`, `models`, `account`, `current_permission_mode`, `capabilities`, ... (fixtures must scrub `account`).
  - `system/init` has `session_id`, `model`, `permissionMode`, `claude_code_version`, `tools`, `capabilities`.
  - The user's settings default to `bypassPermissions` → DL-004 (always pass `--permission-mode`).
  - The CLI modes are `manual|acceptEdits|plan|auto|dontAsk|bypassPermissions` → DL-003.
  - `result` has `terminal_reason` (`completed`), `stop_reason`, `usage`, `total_cost_usd`, `permission_denials`.
  - The user's global hooks run (`system/hook_*` frames): recordings should use `--safe-mode` or `--setting-sources` to keep fixtures free of local setup (decide in S1-2).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 19 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1 (no `web/`, no `ts-rs` types yet; Phase 1 adds them)
- Next: S1-2.

### Round 2 (2026-10-11)

- Committed round 1 as `4973896 feat(spikes): scaffold v2 workspace and S1 Claude protocol core`.
- Review fix: a pending prompt is confirmed only by `command_lifecycle` `started` with the same `command_uuid` (a client UUID on each user message). `system/*`, stream, and assistant frames never confirm an input. A turn without host input (a background task finished, trap 4) gets `origin: provider`. Regression tests in `spikes/s1-claude/src/adapter/tests.rs`: `unrelated_frames_never_confirm_a_pending_prompt`, `provider_turn_does_not_confirm_a_pending_prompt`, `only_the_matching_uuid_confirms`, `replay_echo_is_ignored_once_lifecycle_frames_exist`, `unknown_lifecycle_state_is_counted_not_applied`, `input_started_during_a_turn_folds_into_it`. DL-006.
- S1-2 files: `spikes/s1-claude/src/driver.rs` (`LiveSession`), `spikes/spike-canon/src/fixture.rs` (format + scrubber), `spikes/s1-claude/src/record/` (12 scenarios), `src/bin/record-claude.rs`, `src/replay.rs`, `src/bin/replay-claude.rs`, `tests/replay_fixtures.rs`, 12 fixtures in `spikes/s1-claude/fixtures/rust/`.
- Live recordings: `record-claude --scenario <name>` for all 12 → all `ok` (haiku, `--safe-mode`, about $0.03–0.06 each). The first attempt failed for 7 scenarios: a plain `echo` is read-only and needs no approval in `default` mode (fixed: `touch fd-approve.txt && echo approve-ok`); haiku used the Write tool for the long-text prompt (fixed: "Do not use any tools"); `sleep 30` was blocked and the model moved it to the background (fixed: a foreground `python3 -c "import time; time.sleep(40)"`). The background run gave trap-4 evidence: `system/task_notification` started a new turn with no host input.
- Evidence (input for S1-4 and ADR-001):
  - Prompt confirmed = `command_lifecycle` `started`. `queued` arrives first; `completed` arrives after the turn's `result`. The `cancelled` frame of an interrupted input arrives only when the next input is written (`turn_interrupt`).
  - Queue: a second prompt during a turn runs as its own turn (`queued_turn`: 2 turns).
  - Steer: a prompt during a running tool joins the running turn after the tool result (`message_steering`: 1 turn; the reply ends with "banana").
  - Interrupt: `control_response` `{"still_queued":[]}`, then `result` `error_during_execution` with `terminal_reason` `aborted_streaming` (text) or `aborted_tools` (tool). After `aborted_tools` the CLI exits with code 1 when stdin closes.
  - Restart: an open `can_use_tool` request does not survive a kill. The resumed process reports `pending_permission_requests: []` and does not ask again without input. After a new prompt the model calls the tool again, and the CLI sends a new request with a new native id (`approval_restart`).
  - Native ids: the `can_use_tool` `request_id` is a UUID. Options: `allow`, `deny`, plus `allow_always` when `permission_suggestions` is not empty. The CLI echoes the host's `control_response` on stdout; the core ignores it.
  - Modes: the wire name is `default` for the flag `manual` (DL-003 update).
- Decisions: DL-003 updated, DL-005 (`--safe-mode` recordings), DL-006 (prompt confirmation).
- Commands and results:
  - `cargo run -p s1-claude --bin replay-claude` → 12 fixtures ok, unknown=0
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 33 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S1-3 (TypeScript SDK reference recordings).

### Round 3 (2026-10-11)

- Committed round 2 as `8a114f5`.
- Review fix: `replay` now checks that every write of a process is in the recording **before** `exit` (it no longer drops them). Regression tests in `spikes/s1-claude/tests/replay_fixtures.rs`: `a_write_missing_from_the_recording_fails_before_exit`, `a_dropped_write_in_a_restarted_session_is_not_hidden_by_the_exit`, `a_recorded_write_that_the_core_does_not_make_fails`, `a_changed_write_fails`.
- Done: S1-3. SDK recorder (live, haiku, `--safe-mode`, temp dirs; `bun record.ts all` → 10 ok). Reference replay `spikes/s1-claude/src/reference.rs` + `tests/reference_fixtures.rs` (6 tests).
- SDK findings (evidence for ADR-001):
  - SDK args: `--output-format stream-json --verbose --input-format stream-json --model=haiku --permission-prompt-tool=stdio --permission-mode=default --include-partial-messages` (no `--replay-user-messages`).
  - SDK user messages have no `uuid` → no `command_lifecycle` frames → no per-input confirmation in the SDK stream (DL-008). Our adapter must keep sending a `uuid`.
  - The SDK sets `CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS` → `system/session_state_changed` (`running`/`requires_action`/`idle`); the core parses it as a system frame (0 unknown frames).
  - SDK permission answers carry `toolUseID` → DL-007: our answers now do too; the 6 Rust fixtures with approvals were re-recorded. `approval_restart` now names the command again in the resumed prompt (the model did not repeat it on its own once).
  - Steering via the SDK behaves like ours: the steer prompt joins the running turn.
  - The turn outcomes of every shared scenario are equal for SDK and Rust frames (`sdk_and_rust_recordings_end_turns_the_same_way`).
- Commands and results:
  - `bun record.ts all` (live, spike) → 10 ok; `record-claude --scenario <6 approval scenarios>` (live) → 6 ok
  - `grep -lE "/home|<user name>" spikes/s1-claude/fixtures/*/*.jsonl` → none
  - `cargo run -p s1-claude --bin replay-claude` → 22 ok (12 rust + 10 sdk), unknown=0
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `cargo nextest run --workspace` → 43 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S1-4 remainder (`insta` snapshots of the canonical events, evidence write-up).

### Round 4 (2026-10-11)

- Review feedback: "continue: S1-4 canonical-event snapshots and evidence write-up; make the SDK recorder exit nonzero on scenario failure and terminate timed-out sessions."
- Committed round 3 as `a1e2389`.
- SDK recorder (`sdk-recorder/record.ts`): every session has an `AbortController`; a scenario that times out (turn limit 120 s, scenario limit 300 s) is written as `<name>.failed.jsonl`, its sessions are aborted and noted ("host aborted the session"), and the process exits 1 if any scenario failed or is unknown. `sdk-recorder/selftest.sh` proves it offline with a fake CLI (`exec sleep 600`): unknown scenario → exit 1; timed-out scenario → exit 1, `.failed.jsonl` with the abort note and the `exit` record, fake process gone → `ok sdk-recorder selftest`.
- Snapshots: `tests/event_snapshots.rs` + 22 snapshots (`rust__*`, `sdk__*`).
- Core fixes found by the snapshots and the evidence work:
  - `command_lifecycle` frames arrive before the first `system/init`; the core now holds those events so that the stream opens with `session.started` (`events_before_init_follow_session_started`, `held_events_are_flushed_on_exit`).
  - `allow_always` echoed all `permission_suggestions`, including a `localSettings` destination (writes into the workspace), `setMode`, and `addDirectories`. It now sends session-scoped allow rules only (DL-009; `allow_always_sends_session_scoped_rules_only`, `no_allow_rule_means_no_allow_always`).
- Evidence: `tests/evidence.rs` (6 tests) + `spikes/s1-claude/EVIDENCE.md` (E1 prompt confirmed = `command_lifecycle` `started` for the input's `uuid`; E2 queue/steer timing; E3 open requests do not survive a restart; E4 no native option ids, `behavior` allow/deny, CLI UUID request ids; E5 every §14 S1 row; E6 observed CLI behaviour).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 55 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 22 ok, unknown=0
  - `spikes/s1-claude/sdk-recorder/selftest.sh` → ok sdk-recorder selftest
  - `web/` checks, TS-types drift → not applicable in M1
- Next: S1-5 ADR-001.

### Round 5 (2026-10-11)

- Review feedback: "continue: S1-5 ADR-001 with Proposed status, covering every S1 Must prove row, provider effect states, and crash boundaries."
- Committed round 4 as `da057cd`.
- ADR-001 (`docs/v2/adr/ADR-001-native-claude-adapter.md`, Status: Proposed): process model and arguments, code structure, host commands → wire frames, CLI frames → canonical events, approvals/questions (no native option ids; `allow` / `allow_always` / `deny`), modes, interrupt, resume/fork, terminal paths, traps 1–10; §3 effect states ("confirmed" per effect type), a crash-boundary table with transcript reconciliation, open requests do not survive a restart; §4 fixture set (all 8 shared scenarios supported, none unsupported); §5 one row per §14 S1 "Must prove" with its evidence.
- New live recordings (haiku, `--safe-mode`, temp dir) for the matrix rows: `fork_session`, `plan_capture`, `subagent`, `background_task`, `file_edit`, `ask_user_question`, `interrupt_right_after_send`; `approval_restart` re-recorded with a transcript note.
- Findings: the subagent tool is `Agent` and runs async (it reports after the parent turn's `result` and starts a provider-origin turn); background Bash drains after the turn and its `task_notification` starts a provider-origin turn; plan mode writes the plan to `~/.claude/plans/` and sends it as the `ExitPlanMode` input; the CLI transcript stores each prompt under the client `uuid` (reconciliation key for `outcome_unknown`).
- Core changes: open tool nodes close on `result` and on exit (trap 6, `terminal_paths_close_open_tools`); `background.updated` spike event (DL-011); `approval.expired` reasons `turn_ended` / `cancelled_by_provider` documented (DL-010).
- Fixture hygiene: a home path and the temp dir name were split across streamed deltas and passed the scrubber. `record/coalesce.rs` merges such delta runs before scrubbing; `tests/fixture_hygiene.rs` checks raw frames and joined deltas.
- Evidence: `tests/capabilities.rs` (8 tests) + EVIDENCE.md E7.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 67 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok (19 Rust, 10 SDK), unknown=0
  - `spikes/s1-claude/sdk-recorder/selftest.sh` → ok sdk-recorder selftest
  - `web/` checks, TS-types drift → not applicable in M1
- Next: S2-1 Codex protocol core.

### Round 6 (2026-10-11)

- Committed round 5 work first: `2c3634b docs(adr): ADR-001 native Claude adapter with capability evidence recordings`.
- Review fix: ADR-001 §3.2 now treats the transcript lookup as one-sided. A uuid found in the provider transcript confirms the effect; a uuid not found keeps the effect `outcome_unknown` (the CLI can crash between its read and its transcript write). Never resend automatically. EVIDENCE.md E7 matches.
- Done: S2-1.
  - Live probes of `codex app-server` (codex-cli 0.160.0, `gpt-5.6-luna`, effort low, temp dirs, `untrusted/workspace-write`): handshake (`initialize` → `initialized`), `thread/start`, `turn/start` with `clientUserMessageId`, the echo `item/started userMessage {clientId}`, `item/commandExecution/requestApproval` with integer id 0 and `availableDecisions` (`accept`, `{acceptWithExecpolicyAmendment}`, `cancel`), reply `{decision}` → `serverRequest/resolved`, `item/tool/requestUserInput` (experimental API, plan collaboration mode) answered with `{answers: {<id>: {answers: [..]}}}`.
  - Finding: `turn/start` while a turn runs returns the running turn id and the new user message joins that turn (it acts as a steer). The stable API has no follow-up queue (`thread/queue/*` is experimental), so the core queues follow-ups host-side and sends each one after `turn/completed` (the PLAN §6.2 degradation).
  - Core semantics: a prompt is `received` when the `turn/start` reply arrives and `started` (confirmed) only on its own `userMessage` item (`clientId` == input id, own thread); interrupts before the native turn id are held and sent with the reply (trap 3); approvals keep native decision values as option ids, refuse edited input, are idempotent, and expire on `serverRequest/resolved` without an answer (`cancelled_by_provider`), at turn end (`turn_ended`) and on exit (`process_lost`); async `agentMessage` questions are answered by a new user message and survive the turn end (L4 message channel); unknown server requests get a JSON-RPC error, elicitations a `decline` (fail closed); `usageLimitExceeded` → `usage_limit` (trap 7); `turn/diff/updated` paths dedupe with `fileChange` items.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 90 passed (s2-codex: 23)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0 (S1 unchanged)
  - `web/` checks and TS-types drift check → not applicable in M1
- Next: S2-2 Codex live driver + `record-codex` + fixtures.

### Round 7 (2026-10-11)

- Committed round 6 as `2f87b73`.
- Review fixes (S2 core):
  - Answer idempotence after resolution and turn end: closed requests keep their receipt; a late answer returns the first receipt with `duplicate: true` and writes nothing; a late answer to an expired, unanswered request returns `AdapterError::Expired`. Test `answers_stay_idempotent_after_resolution_and_turn_end`.
  - Child threads: `turn/started` and `turn/completed` of another thread are ignored, and a second native turn id never replaces the running turn. Tests `child_thread_turns_never_replace_the_parent_turn`, `a_second_native_turn_never_replaces_the_running_one`.
- S2-2: `driver.rs` (tokio process driver), `record/` (`record-codex`, 15 scenarios, scrubber, rollout notes), `replay.rs` + `replay-codex`; 15 fixtures in `spikes/s2-codex/fixtures/` recorded live (`gpt-5.6-luna`, effort low, temp dirs).
- Bugs found by the recordings and fixed in the core, each with a test:
  - Trap 3: `turn/interrupt` after the `turn/start` reply but before `turn/started` fails with -32600 "no active turn to interrupt". The core now holds the interrupt until `turn/started` (`interrupt_before_the_turn_id_is_held_then_sent`, fixture `interrupt_right_after_send` re-recorded: turn-1 interrupted).
  - Relative `turn/diff/updated` paths are joined with the thread cwd before the duplicate check (`relative_diff_paths_join_the_thread_cwd_before_the_duplicate_check`).
  - Empty commentary messages are not reported (`empty_agent_messages_are_not_reported`).
  - `thread/goal/cleared` and `thread/settings/updated` are known, ignored notifications.
- Evidence (`spikes/s2-codex/tests/evidence.rs`): prompt confirmed = `userMessage` item with the client id; the rollout keeps every client id; queued prompts = separate turns; steer joins the running turn; an open approval does not survive a restart (`process_lost`, the resumed process does not ask again); native option ids `accept`, `acceptWithExecpolicyAmendment`, `cancel` (no `decline` under `untrusted`); RPC question channel; held interrupt; interrupt during a tool.
- DECISIONS-LOG: DL-014 (recordings use the user's `~/.codex`), DL-015 (host-side follow-up queue; deny = `decline` if offered, else `cancel`).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 109 passed (s2-codex: 42)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
- Next: S2-3 ADR-002.

### Round 8 (2026-10-11)

- Committed round 7 as `7453425`.
- S2-3: `docs/v2/adr/ADR-002-native-codex-adapter.md` (Status Proposed): protocol source (DL-012), process model, host commands, frame → event map, approvals, **both L4 question channels** (RPC reply for `item/tool/requestUserInput`; new user message for `agentMessage` async questions), fail-closed rules, idempotence, modes, interrupt, resume, traps 1–10, **effect states** (what "confirmed" means per effect), **crash boundaries** with one-sided history lookup (client id found → confirmed; not found → stays `outcome_unknown`; never auto-resend), open requests survive restart: no, fixture set, PLAN §14 S2 coverage table (every cited test exists: checked with a grep loop over the ADR).
- New tests:
  - `tests/schema_conformance.rs`: a JSON-schema subset checker validates every recorded frame against the vendored 0.160.0 schema; host writes strictly (`host_writes_conform_strictly`), provider frames for required members and types (`provider_frames_conform`), and a negative test (`the_checker_rejects_a_misspelled_parameter`).
  - `tests/evidence.rs`: `initialize_and_thread_start_open_the_session`, `thread_resume_keeps_the_thread_and_its_context`, `file_change_approvals_use_native_decisions`, `diff_notifications_report_each_file_once`, `plan_mode_reports_the_plan`, `the_resume_reply_lists_earlier_prompts_by_client_id`.
  - `tests/schema_methods.rs`: `async_question_fields_exist_in_the_schema`.
- Core fix found by the new diff test: a scrubbed or already cwd-relative path was joined with the cwd twice; `file_changed` now joins only paths that are relative and do not start with the cwd.
- Core tests split to stay readable: `src/adapter/tests.rs` (625 lines) and `src/adapter/tests_review.rs` (review regressions).
- Live probe: `codex app-server --enable send_message_to_user_async` does not give the model the `request_user_input_async` tool in 0.160.0, so the message question channel has no live fixture; the unit test and the schema test cover it (README + ADR-002 §2.6 say so).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 119 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-1 link identity + address rules.

### Round 9 (2026-10-11)

- Committed round 8 first: `707a04d docs(adr): ADR-002 native Codex adapter with schema conformance and evidence tests`.
- Review fixes (S2):
  - Async message-channel questions: every question of an `agentMessage` is now its own request (one answer each; with more than one question the message names the question). Tests `every_async_question_becomes_its_own_request`, `a_single_async_question_gets_the_bare_answer`.
  - RPC question answers are validated before the write (non-empty map, only asked ids, `{"answers":[strings]}` values); a bad answer writes nothing and the request stays open. Test `rpc_question_answers_are_validated_before_the_write`.
  - `tests/schema_conformance.rs`: provider responses to every client request are checked against `<Method>Response` (`provider_responses_conform`), `additionalProperties` is enforced (`the_checker_validates_answer_map_values`), and a response without a required member fails (`the_checker_rejects_a_response_without_a_required_member`). ADR-002 §2.1/§2.6/§5 updated.
- S4-1 done: new crate `spikes/s4-link` (`secret`, `addr`, `identity`, `tls`, `net`) and `tests/tls.rs` (11 loopback handshake tests).
- Finding: TLS 1.3 session resumption skips the client and server certificate checks, so a revoked key could reconnect with a resumed session. `a_revoked_key_is_refused_on_the_next_handshake` failed until both configs turned resumption off → DL-017.
- DL-016 (rustls + ring provider, tokio-rustls, rustls-webpki), DL-017 (no `0.0.0.0`/`::` bind, accepted-peer address check, no TLS resumption).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 155 passed (s4-link: 30)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-2 link handshake + channels.

### Round 10 (2026-10-11)

- Committed round 9 first: `e02206e feat(spikes): S4 link identity, D13 address rules and pinned-key mTLS`.
- S4-2 done in `spikes/s4-link`:
  - `src/hello.rs` (+ `src/hello_tests.rs`, 8 tests): `Hello {proto_min, proto_max, features, role, node_id, link_epoch, nonce, channel}`, `negotiate()` (highest common version, feature intersection, required `control.v1`, identity/role/channel/nonce checks), transcript over the TLS exporter (`EXPORTER-fleet-deck-link-v1`), both raw hellos and both keys, Ed25519 `auth` signatures with a side tag, `ready` and `close` frames with stable reason codes.
  - `src/frames.rs` (5 tests): control frames `request/response/event/ack/snapshot/ping/pong/close`, 1 MiB limit, method table with scopes and feature gates, `authorize()`.
  - `src/mux.rs` (5 tests incl. a property test): data-channel stream multiplexer with credit flow control.
  - `src/link/` (`mod.rs`, `handshake.rs`, `control.rs`, `data.rs`): `dial()` (control, then data when `data_streams.v1` is negotiated, same epoch), `listen()` with a per-peer link table (higher epoch supersedes with `superseded`, lower or equal → `stale_link_epoch`, data needs the control channel of the same epoch), request/response in both directions with scope checks per request, ping/pong, data streams.
  - `tests/link.rs` (9 tests): `the_station_dials_the_deck`, `the_deck_dials_the_station`, `no_common_version_fails_on_both_sides_with_the_same_reason`, `features_are_negotiated_and_gate_methods_and_channels`, `requests_are_checked_against_the_method_table_and_the_granted_scopes`, `a_higher_link_epoch_supersedes_and_a_lower_one_is_refused`, `a_data_channel_needs_its_control_channel`, `the_data_channel_streams_a_mebibyte_each_way_with_flow_control`, `a_reader_that_gives_no_credit_stops_the_writer`.
- DL-018 (tokio-tungstenite + futures-util), DL-019 (spike link scopes and method table), DL-020 (handshake details PLAN §8.2 does not state).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 182 passed (s4-link: 57)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-3 link pairing + reconnect.

### Round 11 (2026-10-11)

- Committed round 10 first: `97a1490 feat(spikes): S4 link handshake, feature negotiation, epochs and control/data channels`.
- Review fix: the data channel now ends with its control channel. `src/link/control.rs` sends its end reason to the data task (`data_stop`); `src/link/data.rs` ends, fails pending writes, and reports `data_channel_lost` to the control task when it ends first. Because the two TCP connections race, a control task that sees `data_channel_lost` waits up to 500 ms for a peer `close` frame, so a superseded dialer reports `superseded`. Regression tests in `tests/link.rs`: `closing_the_control_channel_ends_the_data_channel_on_both_sides`, `a_superseded_link_loses_its_data_channel_too`, `dropping_the_data_handles_keeps_the_link_up`. 20 repeated runs of `tests/link.rs`: 0 failures.
- S4-3 done in `spikes/s4-link`:
  - `src/pairing.rs` (5 tests): `Invitation` (`fdinv1:wss://<ip>:<port>#<fingerprint>.<secret>.<expiry>`, `Secret` with redacted `Debug`), HMAC-SHA256 proofs over the handshake transcript in both directions, `Invitations` store (single use, 15 min, failure cap 5).
  - `src/link/pair.rs` + `Channel::Pair`: `link::pair()` (joiner) and the listener side; `PeerPolicy::PinnedOrInvited` lets unknown keys through TLS only while an invitation is open, and only onto the pair channel.
  - `src/reconnect.rs` (3 tests): `Backoff` (equal jitter, 500 ms → 5 min cap, reset after 60 s uptime), `EpochClock`, `spawn_dialer()`.
  - `src/bin/fd-link-node.rs`: one deck or station process with its own data directory (identity key, `peers.json`).
  - `tests/pairing.rs` (8 tests) and `tests/multiprocess.rs` (3 tests: `nat_stations_pair_dial_and_reconnect_after_a_deck_restart`, `the_deck_joins_a_station_invitation_and_dials_it`, `public_and_wildcard_addresses_are_refused`). 8 repeated runs of both files: 0 failures.
- DL-021 (pairing details), DL-022 (backoff and epoch source).
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 204 passed (s4-link: 79)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, unknown=0, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, unknown=0, exit 0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: S4-4 ADR-004.

### Round 12 (2026-10-11)

- Committed round 11 as `dcd0010`.
- Review fix: every link setup step after TLS has the bound `NodeConfig::handshake_timeout` (default 10 s): the WebSocket upgrade on the dialer and on the listener, hello/auth, `ready`, the pairing proofs, the wait for the data channel. A timeout is `LinkError::Timeout` with reason `timeout` (DL-023).
- Regression tests `spikes/s4-link/tests/timeouts.rs` (limit 300 ms, a TLS peer that never upgrades): `a_dial_to_a_server_that_never_upgrades_times_out`, `pairing_with_a_server_that_never_upgrades_times_out`, `the_reconnect_supervisor_keeps_retrying_a_stalled_server`, `a_client_that_never_sends_the_upgrade_does_not_block_the_listener`. Stress: 6 runs of `--test timeouts --test link --test pairing` → 0 failures.
- S4-4: `docs/v2/adr/ADR-004-link-protocol-v1.md` (Status Proposed): layers, D13 scope, identity/TLS, channels, handshake + transcript signature + feature flags, control frames + scopes/method table, data mux + credit, pairing, epochs, reconnect, reason codes, failure behaviour, §14 S4 coverage table (every cited test name checked with grep), alternatives, risks, Phase 3 work, owner points (adversary DESIGN REVIEW not run: the loop forbids subagents).
- README: ADR-004 link, timeouts command, whole-spike command.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 208 passed (s4-link 83)
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 15 ok, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok, exit 0
  - `web/` checks and TS-types drift → not applicable in M1
- Next: M1-F close-out (capability matrix in PLAN §6.2 with values + evidence, acceptance run).

### Round 13 (2026-10-11)

- Committed round 12 as `a144f0d`.
- Review fix (post-upgrade timeouts): a stalled `hello`/`auth`, `ready`, or pairing proof now fails with `LinkError::Timeout` (reason `timeout`, the error names the step), the timed-out side sends `close` `timeout` to the peer, and every cleanup write (`close` frames, WebSocket close) has a 1 s bound (`CLEANUP_LIMIT`). New `spikes/s4-link/tests/stalled_handshake.rs` with scripted fake peers that upgrade the WebSocket and then stall: `a_listener_that_never_sends_hello_times_out_the_dial`, `a_listener_that_never_sends_ready_times_out_the_dial`, `a_dialer_that_never_sends_hello_times_out_on_the_listener`, `an_inviter_that_never_answers_the_proof_times_out_the_joiner`, `a_joiner_that_never_sends_its_proof_times_out_and_keeps_the_invitation`. DL-023 and ADR-004 §2.5/§3/§4 updated.
- Flake found and fixed: one stress run failed `multiprocess::nat_stations_pair_dial_and_reconnect_after_a_deck_restart`. Cause: Linux TCP self-connect while the deck restarts (a dial to a free loopback port can get that port as its own source port and hold it). `net::dial_tcp` now refuses a self-connected socket (`NetError::SelfConnect`, reason `self_connect`); tests `net::tests::a_socket_connected_to_itself_is_refused`, `a_normal_connection_is_kept`.
- M1-F capability evidence for the three Codex gaps, recorded live (gpt-5.6-luna, temp dirs): `fork_thread` (`thread/fork`), `subagent` (collab agent; child-thread frames stream on the same connection), `background_terminal` (no notification; found by `thread/backgroundTerminals/list` after the turn: 1, then 0). Core additions: `SessionSpec.fork`, `HostCmd::ListBackground`, child-thread linking when `spawnAgent` completes (`spawn_agent_links_its_child_thread_when_the_item_completes`).
- New matrix tests: `spikes/s1-claude/tests/matrix.rs` (7) and `spikes/s2-codex/tests/matrix.rs` (7).
- PLAN §6.2 matrix: every Claude/Codex value and every Evidence cell filled (only those columns changed; a script checked that Group, Capability and Degradation are unchanged). Every cited test name and fixture exists (checked with grep). DL-024 records the design consequences (Codex background polling, child-thread linking, provider-origin turns, no Codex cost, no restart survival).
- ADR-002: `thread/fork`, `list_background`, child-thread linking, traps 4/5 with evidence, 18 fixtures, `tests/matrix.rs`. ADR-001: `tests/matrix.rs`. README: matrix test commands and the three new Codex fixtures.
- Commands and results:
  - `cargo fmt --all --check` → pass
  - `cargo clippy --workspace --all-targets -- -D warnings` → pass
  - `CI=true cargo nextest run --workspace` → 230 passed
  - `cargo deny check` → advisories ok, bans ok, licenses ok, sources ok
  - `cargo run -p s2-codex --bin replay-codex` → 18 ok, exit 0
  - `cargo run -p s1-claude --bin replay-claude` → 29 ok (19 Rust + 10 SDK), exit 0
  - Clean checkout check: a temporary `git worktree` of HEAD plus this round's diff and new files, each command's exit code captured (no pipes): `cargo fmt --all --check` exit 0; `cargo clippy --workspace --all-targets -- -D warnings` exit 0; `CI=true cargo nextest run --workspace` exit 0, 230 passed; `cargo deny check` exit 0; `replay-claude` exit 0, 29 ok; `replay-codex` exit 0, 18 ok; the README per-spike commands exit 0 (s1-claude 24, s2-codex 38, s4-link 90 passed)
  - `web/` checks and TS-types drift → not applicable in M1 (no `web/`, no `ts-rs` types until Phase 1)
- M1 acceptance (goal text): every item has file and command evidence (see the round 13 summary). The ADRs stay **Proposed** as the goal requires. PLAN Phase 0 *exit* is a separate owner gate: approve ADR-001/002/004, run the adversary DESIGN REVIEW of ADR-004 (the loop forbids subagents), do a real tailnet check, and review DL-001..DL-024.

### Round 14 (2026-10-11)

- Review feedback: fix the Codex background tools that were closed as failed too early (and their later success discarded); add fixture regression tests; make the reviewer's acceptance commands run inside its sandbox.
- Committed round 13 as `4730c13`.
- Codex background tools (DL-025, ADR-002 §2.10 trap 4/6 rows): after a normally completed turn, open `commandExecution` items stay open as background work and close with the real status of their later `item/completed`. Other open tools still close as `failed`, an interrupted/failed turn closes its commands, and process exit closes everything. Tests:
  - fixture tests `matrix::a_background_command_completes_after_the_turn_with_its_real_status` (`background_terminal`: one `tool.completed ok`, after `turn.completed` and the first `background.updated`) and `matrix::a_command_of_an_interrupted_turn_closes_with_the_turn` (`interrupt_during_tool`)
  - unit tests in `src/adapter/tests_review.rs`: `a_command_that_outlives_a_completed_turn_stays_open_until_its_item_completes`, `a_background_command_keeps_its_failure_status`, `background_commands_close_when_the_process_ends`, `a_command_of_an_interrupted_turn_closes_with_it`
- Sandbox (reproduced with `codex sandbox -P :workspace -C . -- <cmd>`: no IP sockets, no Unix socket bind/connect, `~/.cargo` read-only, FIFOs allowed). Before: `cargo deny check` exit 1 (advisory db lock on a read-only path), `cargo nextest run --workspace` exit 100 (s4-link). Fixes (DL-025):
  - `deny.toml`: `db-path = "target/advisory-dbs"` (fresh clone: `cargo deny fetch` once with network)
  - `spikes/s4-link/src/transport.rs`: `FD_LINK_TRANSPORT` = `tcp` | `fifo:<dir>` | `auto`; `.cargo/config.toml` sets `auto` (TCP where allowed, else FIFO pairs for loopback addresses); unit tests `fifo_pairs_connect_carry_bytes_both_ways_and_end_with_eof`, `an_accept_fifo_of_a_killed_listener_is_taken_over`
  - two real defects that the FIFO runs exposed, fixed for every transport: the data channel writes in a separate writer task (no two-writer deadlock when the socket buffer is smaller than the credit window); the listener reads an unpaired dialer's Hello before it sends `not_paired` (no broken pipe instead of the reason)
- ADR-004 (layers table, one-lifetime/writer note, `not_paired` step), README offline section (sandbox notes, `cargo deny fetch`, `FD_LINK_TRANSPORT`).
- Commands and results:
  - inside `codex sandbox -P :workspace`, each exit code captured: `cargo fmt --all --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `CI=true cargo nextest run --workspace` 0, 240 passed; `cargo deny check` 0; `replay-claude` 0, 29 ok; `replay-codex` 0, 18 ok; README per-spike commands 0 (s1-claude 24, s2-codex 40, s4-link 94 passed, s4-link over FIFOs)
  - clean checkout (temporary `git worktree` of HEAD + this round's diff and new files, advisory db copied as `cargo deny fetch` would): fmt 0, clippy 0, `CI=true cargo nextest run --workspace` 0 (240 passed), `cargo deny check` 0, `replay-claude` 0 (29 ok), `replay-codex` 0 (18 ok), `FD_LINK_TRANSPORT=fifo:<dir> cargo nextest run -p s4-link` 0 (94 passed)
  - s4-link with `FD_LINK_TRANSPORT=fifo:<dir>` 3 runs and with TCP: 94 passed each
  - `web/` checks and TS-types drift → not applicable in M1
- M1 acceptance: all items keep their evidence (round 13 summary); DECISIONS-LOG now DL-001..DL-025. Phase 0 exit stays an owner gate (ADR approval, adversary DESIGN REVIEW of ADR-004, real tailnet check).

### Round 15 (2026-10-11)

- Review feedback: allow the cargo checks and the offline replays in the review-command allowlist; `cargo nextest` was rejected before it could run, so the reviewer could not accept independently.
- Committed round 14 as `899a52d`.
- The reviewer's allowlist is in its home directory (`~/.codex/rules/`), outside the repository. The goal forbids writes there, so this round makes the acceptance runnable from inside the repository (DL-026):
  - `.codex/rules/fleet-deck-acceptance.rules`: project execpolicy rules (loaded by Codex in a trusted project) that allow only the offline acceptance commands. `codex execpolicy check -r .codex/rules/fleet-deck-acceptance.rules <cmd>` → `allow` for `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo nextest run --workspace`, `cargo test --workspace`, `env CI=true cargo nextest run --workspace`, `cargo deny check`, `cargo run [-q] -p s1-claude --bin replay-claude`, `cargo run [-q] -p s2-codex --bin replay-codex`, `sh spikes/acceptance.sh`; no match for `record-claude`, `record-codex`, `rm -rf target`, other scripts
  - `spikes/acceptance.sh`: one command for all six acceptance commands, each exit code printed, non-zero exit on any failure; cargo-nextest if installed, else `cargo test --workspace` (`FD_TEST_RUNNER=test` forces it)
  - README offline section: the script, the runner fallback, the rules file
- Commands and results:
  - `sh spikes/acceptance.sh` → exit 0 (fmt 0, clippy 0, `cargo nextest run --workspace` 0 with 240 passed, deny 0, replay-claude 0 with 29 ok, replay-codex 0 with 18 ok)
  - `FD_TEST_RUNNER=test sh spikes/acceptance.sh` → exit 0 (`cargo test --workspace`: 32 test binaries, 240 passed, 0 failed)
  - the same two runs inside `codex sandbox -P :workspace -C .` → exit 0 each
  - the same two runs on a clean checkout (temporary `git worktree` of HEAD + this round's diff and new files, advisory db copied as `cargo deny fetch` would) → exit 0 each, 240 passed
  - `web/` checks and TS-types drift → not applicable in M1
- M1 acceptance: all items keep their evidence; DECISIONS-LOG DL-001..DL-026. Phase 0 exit stays an owner gate (ADR approval, adversary DESIGN REVIEW of ADR-004, real tailnet check).
