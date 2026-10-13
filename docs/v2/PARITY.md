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
| Revive | Native resume | `crates/fleetdeck-station/src/manager/resume_tests.rs` `row_resume_reopens_the_same_native_session_with_its_memory`; `crates/fleetdeck-providers/tests/codex_core_replay.rs` `resume_and_fork_start_sessions_on_the_recorded_threads` | done (station, deck API, and the board's Resume and Resume all, DL-140, DL-141) |
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
| Provider version gate | `station_info_providers_are_probed_once_with_versions`; `probe_of_a_missing_binary_reports_unknown`; `version::tests`; `manager/version_tests.rs` | done (DL-162): an older CLI warns, only a deny-listed version is refused (DL-158) |

## Provider-specific capability degradation

`web/src/session/session.test.tsx` "a provider without optional capabilities gets prompt and follow-up only" and "a Codex session shows the controls of its capabilities (P5-3)"; `a_provider_from_a_station_before_p5_3_has_no_capabilities`; `the_capability_stays_off_until_the_live_smoke_passes` (coordinator handoff per provider).

## Deferred (DL-131, owner decision needed)

1. Session grid (many live session views at once).
2. LAN station discovery (mDNS, `mdns-sd`); owners add dial targets by hand.
3. ~~A per-provider minimum CLI version that refuses an older CLI.~~ Replaced by DL-158 and built in DL-162: an older CLI gets a warning, and only a known-broken version is refused.

## Board parity with the v1 board (DL-141, DL-142)

The v1 board's features and their v2 status are in the parity audit of DL-141 (a 139-row matrix). Built in DL-141: the v1 columns and cards, the answerable Needs-you rail with keys, conflict marks, repo grouping, Resume all, Clear and per-browser archive, stars, the bypass mark, the last error, mail to a station, a repo or everyone, compact density, light/dark/system theme, help with concepts and first-run help, the mail key `c`, plan assign to a live session, the batch preview, recent and pinned directories, card mail badges, file chips and the activity sparkline, the global Files page, Markdown view and copy path, TUI Shift+Enter, OSC 52 copy, the deck version, and the read-only worktree list (DL-142). Built in night 2 (DL-143 … DL-150):

| v1 feature | v2 | Tests | Fail-open and exposure proof (R9.2) |
| --- | --- | --- | --- |
| Worktree verdict without running repository programs | DL-143: no `git status`; the station reads the index and hashes changed files itself | `worktrees_tests.rs` (filters, fsmonitor and the signature verifier never run; staged, rewritten, `chmod +x`, unborn and submodule work) | Fail-open: `a_repository_with_git_filters_is_inspected_without_running_them`, `a_configured_fsmonitor_command_does_not_run`. Exposure: `board_worktrees.rs` `the_owner_lists_the_worktrees_of_a_station_and_a_token_cannot`. |
| Git access preflight, branch choice, ticket callsigns | DL-144: `workspace.preflight`, `Worktree { branch, base }`, `<animal>-<KEY>` | `preflight.rs`, `provision.rs` tests; `web/e2e/night2.spec.ts` | Fail-open: `workspace_preflight.rs` `the_preflight_is_relayed_to_the_station_and_its_refusals_are_mapped`. Exposure: `preflight.rs` `urls_with_credentials_or_outside_the_roots_are_refused`. |
| Conflict warning to the agents | DL-145: one follow-up mail per pair, file and recipient; `conflict_mail` in deck.toml | `crates/fleetdeck-deck/tests/conflict_mail.rs` | Fail-open: `conflict_mail.rs` `an_ended_session_gets_no_mail_and_the_live_one_still_does`, `conflict_mail_false_mails_nobody`. Exposure: —. |
| LAN link and QR code for a phone | DL-146: "Add a device" (owner only, single-use link per board address, local QR encoder) | `board_device_link.rs`; `web/e2e/night2.spec.ts` "Add a device …" | Exposure: `board_device_link.rs` `a_token_is_refused_and_no_link_is_made`, `a_request_without_the_csrf_header_is_refused`, `the_eleventh_link_in_a_minute_waits`. |
| Image paste into a TUI or shell tab | DL-147: `files.write_temp` and `POST …/paste`; the path is typed, never Enter | `paste_tests.rs`, `board_paste.rs`, `web/src/terminal/paste.test.ts` | Fail-open: `board_paste.rs` `a_station_without_a_paste_directory_answers_unsupported`. Exposure: `paste_tests.rs` `every_extension_needs_its_magic_bytes`, `a_link_in_place_of_the_directory_is_refused`; `board_paste.rs` `a_body_above_one_mib_is_refused`. |
| Setup command | DL-148: opt-in `allow_setup_cmd`, owner only, process group plus marker cleanup | `setup_tests.rs`, `board_setup_cmd.rs`; `web/e2e/night2.spec.ts` | Fail-open: `manager/setup_tests.rs` `a_slow_setup_does_not_block_other_sessions_and_a_stop_ends_it`. Exposure: `board_setup_cmd.rs` `a_token_cannot_start_a_session_with_a_setup_command`; `setup_tests.rs` `a_coordinator_start_cannot_carry_a_setup_command`, `a_station_with_the_policy_off_refuses_a_setup_command`. |
| Per-browser archive | DL-149: the archive lives on the deck (sync across devices) | `board_archive.rs`, `web/src/ui/archive.test.ts` | Fail-open: `board_archive.rs` `the_owner_archives_and_a_resend_changes_nothing`. Exposure: `archive_changes_need_drive_csrf_and_a_valid_body`. |
| `fleetdeck doctor` | read-only setup check | `crates/fleetdeck-cli/tests/doctor.rs` | Fail-open: `doctor.rs` `a_provider_older_than_the_tested_version_is_a_warning` (a warning, never a failure). Exposure: — (read-only). |
| Worktree removal from the board | DL-150: trash, restore, delete permanently, stop tracking | `trash_tests.rs`, `trash_race_tests.rs`, `board_worktree_trash.rs`, `web/e2e/trash.spec.ts` | Fail-open: `trash_race_tests.rs` `a_folder_replaced_after_the_claim_is_neither_deleted_nor_moved`; `trash_tests.rs` `a_queued_purge_after_a_release_is_superseded_and_deletes_nothing`. Exposure: `board_worktree_trash.rs` `a_token_and_a_missing_csrf_header_are_refused_and_audited`. |
| Models list for the board selector | DL-151: `session.models` event from the provider | `claude/tests.rs`, `codex/tests.rs`, `fold_tests.rs` | Fail-open: an empty or failed `model/list` emits nothing and no error (`codex/tests.rs`). Exposure: — (display only). |
| Provider version check | DL-158, DL-162: `version_status` in `station.info`, spawn-form warning, `doctor` warning, deny list | `version::tests`, `manager/version_tests.rs`, `doctor.rs`, `web/src/spawn/spawn.test.tsx` | Fail-open: `an_older_version_is_older_than_tested` (a warning only), `station_info_marks_an_older_cli_and_accepts_a_tested_one`. Exposure: `a_start_on_a_broken_version_is_refused_with_the_reason`. |

Also built: a fast graceful stop during a turn and on the shim host (DL-141 follow-up), and a chat notice when the v1 plugin is active in a v2 session.
