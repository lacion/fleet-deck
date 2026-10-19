# Owner knobs: the hard-coded limits that become settings

- **Status:** Revision 3 (architect, 2026-10-19), accepted for the build: DESIGN REVIEW round 2 PASS (report `adv-knobs-design-r2-report.md`); its one MAJOR (rule 10 bounds single calls, not whole synchronous operations) is answered by removing the three knobs of synchronous operations (`limits.precheck_seconds`, `limits.git_fetch_seconds`, `limits.integration_seconds`). Revision 2 (architect, 2026-10-19), after DESIGN REVIEW round 1 (report `adv-knobs-design-r1-report.md`: 1 BLOCKER recording CHECK constraints, 2 MAJOR session revival and outer deadlines, 1 MINOR raw logs; all answered below). Revision 1: proposed (architect, 2026-10-19). The owner asked for the recommended set of the knobs inventory (`~/fleetdeck-briefs/knobs-inventory.md`, page "Fleet Deck Knobs") and said: "everything needs to be configurable from the deck, specially stuff that happens on the remote machines". DESIGN REVIEW required before the build (high risk: sign-in lifetimes, password length, elevation, audit retention, agent desktop rates).
- **Scope:** 29 settings: 14 on the deck and 15 on each station. Every other constant stays fixed.

## 1. Rules

1. **Registry fields only.** Each knob is a field of the settings registry (`crates/fleetdeck-core/src/settings/fields.rs`). The board edits it on the deck tab or on the station's tab, over the link for a remote station (DL-172). No knob is an environment variable, a CLI flag or a file that only the station machine can change.
2. **Tables.** The deck knobs live in the `deck.toml` tables `[security]`, `[retention]` and `[limits]`. The station knobs live in the `station.toml` tables `[limits]`, `[retention]` and `[capture]`. The field keys are dotted (`security.min_password_chars`), as `model_help.*` already is. A missing table or key means the default.
3. **Defaults are today's values.** The build changes no behavior while the owner sets nothing.
4. **Bounds.** Each knob has a floor and a ceiling (inclusive). The parser refuses a value outside them, with the existing "out of range" error. The floor is the least safe value that Fleet Deck accepts at all.
5. **Elevation.** A change that weakens a security knob needs the owner's elevation (the existing `danger` rule of the holder, with a direction): a shorter password, a longer login, a longer elevation, a longer token, a longer display session, a longer invitation, a shorter audit history, a higher agent desktop rate. The other direction needs no elevation. The field has the `danger` badge.
6. **Apply.** "Live" knobs act on the next use of the value (a new login, the next command, the next prune). "Restart" knobs act after a restart of the deck or the station, which the board already offers.
7. **Pairs.** `security.login_idle_days` ≤ `security.login_max_days` and `security.display_idle_days` ≤ `security.display_max_days`; a save that breaks a pair is refused with the text "The idle time cannot be longer than the longest time."
8. **No silent clamp.** A value is used as it is saved; a station never changes a value that the deck sent.
9. **Snapshot at start (revision 2).** A timeout or a limit is read once when an operation starts; a change applies to the next operation, never to one that runs.
10. **Only asynchronous operations get a timeout knob (revision 3).** An operation that the deck or the board waits for synchronously keeps its fixed timeouts, because a per-call value cannot bound a whole operation with several calls and a cleanup. The station timeout knobs of §3 are all for asynchronous operations whose state the deck reads later.
11. **Shrinking a lifetime is permanent (revision 2).** A save that lowers `security.login_idle_days` or `security.login_max_days` deletes, in the same apply, every owner and member login session whose idle time or age is over the new limit; a save that lowers `security.display_idle_days` or `security.display_max_days` does the same for display sessions. The existing session revocation then ends their WebSockets, terminal relays and recording leases. Raising a limit later never brings a deleted session back.

## 2. Deck knobs (`deck.toml`, deck tab)

| Key | Default | Floor–ceiling | Apply | Elevation when | Replaces |
|---|---|---|---|---|---|
| `security.min_password_chars` | 12 | 8–128 | Live (next password set) | lowered | `MIN_PASSWORD_CHARS` (deck `store/auth.rs:36`) |
| `security.login_idle_days` | 7 | 1–90 | Live | raised | `SESSION_IDLE_MS` (`store/auth.rs:30`) |
| `security.login_max_days` | 30 | 1–365 | Live | raised | `SESSION_MAX_MS` (`store/auth.rs:32`) |
| `security.elevation_minutes` | 10 | 1–60 | Live (next elevation) | raised | `ELEVATION_TTL_MS` (`elevation.rs:23`) |
| `security.api_token_max_days` | 90 | 1–365 | Live (next token) | raised | `MAX_TOKEN_TTL_MS` (`store/auth.rs:38`) |
| `security.display_idle_days` | 30 | 1–365 | Live | raised | `DISPLAY_IDLE_MS` (`store/displays.rs:24`) |
| `security.display_max_days` | 90 | 1–730 | Live | raised | `DISPLAY_MAX_MS` (`store/displays.rs:26`) |
| `security.invite_minutes` | 15 | 5–1440 | Live (next invitation) | raised | `INVITE_TTL` (`link/runtime.rs:46`) |
| `security.desktop_screenshots_per_minute` | 2 | 1–60 | Live | raised | proto `SCREENSHOTS_PER_WINDOW` as enforced by the deck |
| `security.desktop_actions_per_minute` | 30 | 1–300 | Live | raised | proto `MUTATIONS_PER_WINDOW` as enforced by the deck |
| `retention.audit_days` | 90 | 7–3650 | Live (next prune) | lowered | `AUDIT_RETENTION_MS` (`store/audit.rs:12`) |
| `retention.audit_max_rows` | 200000 | 10000–10000000 | Live (next prune) | lowered | `AUDIT_MAX_ROWS` (`store/audit.rs:15`) |
| `limits.delivery_hours` | 24 | 1–168 | Live (next mail or task) | never | `DELIVERY_TTL_MS` (`http/fleet_routes.rs:34`) |
| `limits.radar_minutes` | 5 | 1–60 | Restart | never | `RadarOptions.every` (`fleet_git_radar.rs:53`) |

Notes:
- A shorter `security.login_*` or `security.display_*` deletes the sessions over the new limit at the save (rule 11).
- The board client has its own fixed request timeouts, so the deck's waits for the station stay fixed (revision 2 removed `limits.station_wait_seconds`).
- The two desktop rates replace only the deck's per-grant per-minute counters for screenshots and mutations. The other desktop limits (in flight, images, input) stay fixed.

## 3. Station knobs (`station.toml`, each station's tab)

| Key | Default | Floor–ceiling | Apply | Elevation when | Replaces |
|---|---|---|---|---|---|
| `limits.shell_command_minutes` | 10 | 1–240 | Live (next command) | never | agent `!` command `TIMEOUT` (`manager/shell_run.rs:37`) and the run limit sent to Codex (`codex/shell.rs:22`) |
| `limits.setup_minutes` | 10 | 1–240 | Live | never | setup `DEFAULT_TIMEOUT` (`workspace/setup.rs:35`) |
| `limits.land_check_minutes` | 10 | 1–240 | Live | never | land `CHECK_TIMEOUT` (`manager/land_run.rs:31`) |
| `limits.clone_minutes` | 15 | 1–240 | Live | never | `CLONE_TIMEOUT` (`workspace/provision.rs:30`) |
| `limits.git_push_seconds` | 60 | 10–600 | Live | never | `PUSH_TIMEOUT` and `LandTimeouts.push` |
| `limits.provider_stop_grace_seconds` | 30 | 5–300 | Restart (revision 3: the adapters copy it at start) | never | `StopPolicy.grace` (`providers/host/mod.rs:132`) |
| `limits.reconnect_max_seconds` | 300 | 5–600 | Restart | never | `BackoffPolicy.cap` of the station's link dialer |
| `limits.scrollback_lines` | 5000 | 500–100000 | Live (new shells) | never | `SCROLLBACK_LINES` |
| `limits.scrollback_mib` | 8 | 1–64 | Live (new shells) | never | `SCROLLBACK_BYTES` |
| `retention.history_days` | 90 | 1–3650 | Restart | lowered | `HISTORY_RETENTION_MS` (`store/mod.rs:142`, via the existing `StoreConfig`) |
| `retention.event_days` | 7 | 1–90 | Restart | never | `EVENT_RETENTION_MS` (`store/mod.rs:137`) |
| `retention.raw_log_mib` | 16 | 0–256 (0 = off) | Restart | raised | `RawLogLimits` (4 files of `raw_log_mib / 4` MiB each) |
| `capture.quota_gib` | 4 | 1–1024 | Live | never | `CAPTURE_QUOTA` (`desktop/capture/mod.rs:43`) |
| `capture.free_reserve_gib` | 2 | 1–100 | Live | never | `FREE_RESERVE` (`desktop/capture/mod.rs:45`) |
| `capture.recording_max_seconds` | 120 | 10–3600 | Live | never | the 120 s recording ceiling (station `MAX_SECONDS`, proto `recording.rs:27`, `coordinator_desktop.rs:318`, the board form) |

Notes:
- `retention.raw_log_mib` turns provider raw logs off at 0. Raw logs can hold prompt text, so every increase (also from 0) needs the elevation.
- **Recording length.** The proto ceiling becomes 3600 s (an absolute check on deck and station). The station enforces its own `capture.recording_max_seconds`: a longer request is refused with the code `recording_too_long` and the fixed text "This recording is longer than this station allows. Shorten it, or raise Longest recording in the station settings." (the board knows the value from `desktop.info` and checks it before it sends) The station reports the value in `desktop.info` (`recording_max_seconds`), and the board's recording form uses it as its maximum. The grant's `recording_second_cap` stays the per-grant budget. The 256 MiB file limit stays: a recording that reaches it is discarded (the existing size result), so the help text of the field says "A long recording that reaches 256 MiB is discarded."
- **Recording length, storage (revision 2).** The SQLite `CHECK (… BETWEEN 1 AND 120)` constraints change to `BETWEEN 1 AND 3600` through forward migrations: the station table of `schema_v36.rs` (station migration V40) and the deck tables of `store/desktop_recordings.rs` and `store/coordinator_input_any_schema.rs` (deck migration V40; `tests/merged_migrations.rs` bumps both). The coordinator MCP schema (`coordinator/desktop_tools.rs`) advertises a maximum of 3600, and the station refuses above its own setting. Tests: a 121-second recording on a fresh database and on an upgraded one.

## 4. Board

- New sections on the settings tabs, in this order after the existing ones: **Security limits** (`security.*`), **Time limits** (`limits.*`), **Retention** (`retention.*`), **Captures and recordings** (`capture.*`). The section of a key is found by its prefix (as `model_help.*`).
- Each field has a label with its unit ("Login idle time (days)") and a help text that names what it limits and its default.

## 5. What stays fixed

Protocol and frame sizes, the mux window and stream count, the token and ID lengths, the handshake limits, the rate limits of sign-in failures, the plan lifetimes, the in-flight limits, the input text limits and every other constant of the inventory. They can be added later with the same rules.

## 6. Build split

| Task | Deliverable |
|---|---|
| **K1** Registry | The tables, keys, defaults, bounds, pairs and elevation rules in `fleetdeck-core` (parse, field tables, holder danger), the TS settings types if any, the board sections; no consumer changes (the values are parsed and shown only). |
| **K2** Deck consumers | The 14 deck knobs read by the deck code instead of the constants, and the session deletion of rule 11 (member sessions have no `last_seen` today, `store/schema.rs:522`: K2 adds idle tracking for them in a deck migration, or deletes by age only for members; decided in the K2 brief). |
| **K3** Station consumers | The station knobs except the recording length, read by the station and provider code. |
| **K4** Recording length | Proto ceiling, the deck and station migrations of the CHECK constraints, the MCP schema, station enforcement and report, deck pass-through, board form maximum. |
| **K5** Docs | The decision log entry, `operations.md` (a "Limits and retention" part with board steps), parity and security rows. |
