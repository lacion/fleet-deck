# v0 → v2 parity record (M3 item 6-13)

This file closes PLAN Phase 6 exit for the parity rows. Each "Keep" row of PLAN §4.1 and each row of the parity table (PLAN §16) has a named automated test, or it is deferred under DL-131 ("deferred: owner decision needed").

Commands: Rust tests run with `cargo nextest run -E 'test(<name>)'`, board unit tests with `bun test` in `web/`, and board flows with `bunx playwright test <file>` in `web/` (phone viewport, scripted fake deck). `make check` runs all of them.

## PLAN §16 parity table

| v0 feature | v2 | Evidence (test) | State |
| --- | --- | --- | --- |
| Live derived status lanes | From canonical events + `unreachable` / `lost` | `fleetdeck-proto` `fold::tests::a_session_moves_through_the_lanes`; `fleetdeck-deck` `a_command_for_an_unreachable_station_waits_and_is_sent_when_it_links`; `crates/fleetdeck-cli/tests/shim_station.rs` `a_dead_shim_leaves_its_session_lost_and_its_claims_outcome_unknown_after_a_station_start` | done |
| Model badge follows `/model` | From session and model events | `fold::tests::a_model_change_moves_the_model_badge_to_the_new_model` (new in 6-13); `web/src/App.test.tsx` "cards show the title, station, provider badge, workspace, current tool, usage and model" | done |
| Conflict radar | Across stations | `web/e2e/conflicts.spec.ts` "two sessions on two stations that change one file show as a conflict" | done |
| Needs-you rail | Approvals answered from the board | `web/e2e/session.spec.ts` "an approval is answered from the needs-you rail" | done |
| Re-armed questions | Not needed (protocol holds the request) | — (PLAN: not needed) | not applicable |
| Mail between sessions | Native delivery through the outbox | `a_mail_is_delivered_once_as_a_framed_follow_up_or_steer`; `web/e2e/fleet.spec.ts` "a mail goes to the chosen session once, as a steer when asked" | done |
| `assign auto` routing | Plus station terms | `web/e2e/assign.spec.ts` "assign auto sends the task once and names the session", "an unrouted task offers a spawn on the best station" | done |
| Spawn, batch, `Nx` | Plus a station picker | `web/e2e/spawn.spec.ts` "a session starts from the form with a picked directory and opens", "a batch starts one worktree session per line with Nx (M3 6-1)"; `web/src/spawn/spawn.test.tsx` "a batch starts one session per line and Nx repeats a task (M3 6-1)" | done |
| Live terminal (xterm.js on tmux) | Session view; separate human shell (D10) | `web/e2e/session.spec.ts` "steer, follow-up, interrupt and the mode ladder send one command each"; `web/e2e/shell.spec.ts` "an admin opens a station shell, types into it, reattaches, and closes it" | done |
| Terminal grid | Session grid | — | deferred (DL-131) |
| Shift+Enter, Ctrl+V screenshot paste | Composer newline; image paste | `web/e2e/parity.spec.ts` "Shift+Enter in the composer adds a newline and sends nothing (v0 parity)" (new in 6-13); `web/e2e/images.spec.ts` "a picked or pasted image goes with the prompt" | done |
| Revive | Native resume | `crates/fleetdeck-station/src/manager/resume_tests.rs` `row_resume_reopens_the_same_native_session_with_its_memory`; `crates/fleetdeck-providers/tests/codex_core_replay.rs` `resume_and_fork_start_sessions_on_the_recorded_threads` | done (station and deck API); board button deferred (DL-131) |
| Remote control (claude.ai) | Dropped | — (PLAN: dropped) | not applicable |
| Plan library | Capture from the protocol | `web/e2e/plans.spec.ts` "a captured plan is read, archived, restored and starts a session" | done |
| Workspace tools (files, worktrees, rename, event feed) | Per station | `web/e2e/revert.spec.ts` "the files of a session are browsed, read and searched; a symlink is not opened"; `a_worktree_start_creates_the_callsign_worktree_on_its_branch`; `web/e2e/workspace.spec.ts` "a session is renamed from the board, also after it ended", "the event feed shows fleet changes newest first, filters them and opens a session" | done |
| Share board on LAN | Deck access on LAN and tailnet + station discovery | `web/e2e/stations.spec.ts` "an admin pairs a listening station, sees a refusal, and removes a station"; `bad_invitations_public_addresses_and_wrong_secrets_pair_nothing` | done for LAN/tailnet access; mDNS station discovery deferred (DL-131) |
| Adopt / takeover a terminal session | "Import session id" | `an_imported_claude_session_continues_the_given_native_session`; `an_import_is_refused_for_a_bad_id_a_foreign_workspace_or_a_native_session_in_use`; `web/e2e/spawn.spec.ts` "a terminal CLI session is imported by its id as a driven session (6-12)" | done |
| Gateway routing (LLM gateway env) | Station provider profiles | `fleetdeck-core` `config::tests::instance_ids_are_checked_and_unique`; `fleetdeck-core` `childenv` `the_instance_adds_back_only_its_own_settings` | done |
| Coder support | Dropped (D13) | — (PLAN: dropped) | not applicable |
| `/fleet` slash command, fleet-doctrine skill | Deck-served control skill for coordinator agents | `web/e2e/fleet.spec.ts` "the owner revokes a coordinator grant from the phone"; ADR-006 grant tests in `crates/fleetdeck-deck/src/store/coordinator_tests.rs`; opt-in live handoff smoke in `crates/fleetdeck-providers/tests/live_smoke.rs` (DL-219, Claude verified, Codex unverified) | done (Claude); Codex coordinator start refused `handoff_unverified` |
| Plugin install + version-bump channel | Binary release | `crates/fleetdeck-cli/tests/packaging.rs` `the_cargo_dist_config_releases_fleetdeck_for_linux_and_macos_with_the_packaging_files` | done (release itself is an owner step) |

## PLAN §4.1 Keep rows

| v0 lesson | Evidence (test) | State |
| --- | --- | --- |
| Status is a projection of events | `fold::tests::a_session_moves_through_the_lanes` | done |
| Normalize at intake, provider-blind core | `crates/fleetdeck-providers/tests/capability_matrix.rs` `claude_capabilities_match_the_matrix`; `a_provider_from_a_station_before_p5_3_has_no_capabilities` | done |
| Contracts first, `schema_version`, version negotiation | `crates/fleetdeck-link/tests/version_skew.rs` `a_deck_and_a_station_one_build_apart_negotiate_the_shared_features`, `an_event_of_a_newer_build_is_kept_as_unknown_and_written_back_unchanged` | done |
| Snapshot + resync after reconnect | `another_incarnation_a_cursor_ahead_or_session_snapshot_gets_a_snapshot_batch`; `a_replay_sends_the_events_after_the_cursor_then_live_events_and_acks_keep_the_link` | done |
| Exact process identity; never signal a foreign process | `a_reused_pid_with_another_start_time_or_boot_id_is_lost_not_reattached` | done |
| Single-flight destructive actions; visible rejection | `a_reused_command_id_with_another_payload_is_rejected`; `web/e2e/spawn.spec.ts` "a refused start shows the station reason and keeps the form" | done |
| Credential hygiene | `the_child_gets_exactly_the_scrubbed_environment`; `an_invitation_round_trips_and_debug_hides_the_secret` | done |
| Numbered, transactional migrations | `the_migrations_are_valid` (deck and station) | done |
| No model calls in the core | `crates/fleetdeck-cli/tests/cutover.rs` `no_crate_calls_a_model_api_directly` (no crate source names a model API endpoint) | done |
| Callsigns, tickets, worktree naming | `every_session_gets_its_own_callsign`; `a_worktree_start_creates_the_callsign_worktree_on_its_branch` | done |
| Batch spawn with `Nx`, forced worktree isolation | `web/e2e/spawn.spec.ts` "a batch starts one worktree session per line with Nx (M3 6-1)" | done |
| Conflict radar | `web/e2e/conflicts.spec.ts` (see above) | done |
| Fake agents and test seams | `crates/fleetdeck-cli/tests/cutover.rs` `normal_tests_use_fake_agents_and_live_smoke_tests_are_opt_in` (the `fleetdeck-testkit` fakes exist; every live smoke test in `crates/fleetdeck-providers/tests/live_smoke.rs` is ignored behind `FD_LIVE_SMOKE=1`, `make smoke-live`) | done |
| Provider version gate | `station_info_providers_are_probed_once_with_versions`; `probe_of_a_missing_binary_reports_unknown` | probe done; a per-provider minimum version deferred (DL-131) |

## Provider-specific capability degradation

`web/src/session/session.test.tsx` "a provider without optional capabilities gets prompt and follow-up only" and "a Codex session shows the controls of its capabilities (P5-3)"; `a_provider_from_a_station_before_p5_3_has_no_capabilities`; `the_capability_stays_off_until_the_live_smoke_passes` (coordinator handoff per provider).

## Deferred (DL-131, owner decision needed)

1. Session grid (many live session views at once).
2. A resume button on the board for an ended session (native resume works through the station and the deck API `session.resume`).
3. LAN station discovery (mDNS, `mdns-sd`); owners add dial targets by hand.
4. A per-provider minimum CLI version that refuses an older CLI (the probe reports the version).
