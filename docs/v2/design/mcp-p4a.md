# MCP P4a — the default worker grant

- **Status:** Draft revision 1, 2026-10-20. Needs an adversary DESIGN REVIEW (authorization, cost) before it freezes.
- **Program:** DL-251 (D1: every session starts with a small grant that can spawn), owner decisions W1–W3 of 2026-10-20 (`~/fleetdeck-briefs/mcp-p3-research-notes.md` §"P4 owner decisions"). P2 is folded into P4; P4b (live grant changes and the listen stream) follows this.
- **Facts:** the start-path map of the P4 research (only the owner's board `session.start` can carry a grant today, through `owner_start`; resume has no spec; coordinator spawns refuse a grant; ADR-008 rule 3 makes every session with a mutating grant a "coordinator").
- **Out of scope:** live changes of a running session's grant and `tools/list_changed` (P4b); a grant on resume, import, bearer-token or CLI starts (W2).

## 1. Goals

1. With the deck setting on (the default, W3), every eligible start (§3) gets a worker grant: classes `read`, `mail`, `drive`, `spawn`, scope `lineage`, the stations, spawn cap, mode ceilings, lifetime and idle time of the setting.
2. A worker grant never chains: a session that a grant starts gets no worker grant.
3. Worker grants are exempt from ADR-008 rule 3 (W1): fleet control can still act on those sessions. Owner-made coordinator grants keep rule 3.
4. A start never fails because of the worker grant: when the station cannot hand off the credential, the session starts without it.
5. The owner can change the spawn cap and the mode ceilings of any grant after creation (deferred from P0).

## 2. The setting (deck)

`deck.toml` section `[worker_grant]` (new `DeckConfig` field `worker_grant: WorkerGrantConfig`, `RawDeck` + `RAW_DECK_KEYS`, settings registry fields, `DeckLive`), with these keys, defaults and bounds:

| Key | Default | Bounds |
| --- | --- | --- |
| `worker_grant.enabled` | `true` (W3) | bool |
| `worker_grant.stations` | `"all"` | `"all"` or a list of 1–16 station names |
| `worker_grant.spawn_cap` | 2 | 0–10 |
| `worker_grant.claude_mode` | `acceptEdits` | a valid Claude ceiling (`valid_ceiling`, P0) |
| `worker_grant.codex_mode` | `on-request/workspace-write` | a valid Codex ceiling |
| `worker_grant.lifetime_hours` | 12 | 1–168 |
| `worker_grant.idle_minutes` | 30 | 5–1440 |

- Elevation (`DeckHolder::danger`) for a change that widens: `enabled` off → on; a higher `spawn_cap`; a higher ceiling (by `mode_within`); `stations` from a list to `"all"` or a list with a station that was not in it; a longer `lifetime_hours` or `idle_minutes`. A narrowing change needs none.
- The board shows the section "Default worker grant" from the field metadata (no new board component beyond the registry).
- The setting is read when a grant is created (§3); a change does not touch existing grants.

## 3. Where the deck attaches a worker grant

A single deck function `worker_grant_for(station, command_id, principal_kind) -> Option<CoordinatorSpec>` builds the spec; it is called on exactly these paths, and only when all of these hold: the setting is on; the command is a `SessionCommand::Start` whose spec has `coordinator: None`; the target station's link negotiated the feature `worker_grant.v1` (§5) and `coordinator.v1`; the station is paired.

| Path | File | Principal kind |
| --- | --- | --- |
| Owner board start (single or batch, not import) | `http/command_routes.rs` `route`, the branch after the owner/coordinator checks | `owner` (owner login only; a member or bearer-token start gets none) |
| Scheduled run | `scheduler/run.rs` (the `submit_as` call) | `schedule` |
| Issue batch start | `project_writes.rs` (the `submit_as` call) | `issue_batch` |

Never: a coordinator `spawn`, `start_in_project`, `assign_auto` spawn, Fleet Git move, resume, fork, import, CLI/station-local start, a start that already has an owner coordinator request (that grant is the session's only grant).

`worker_grant_for` creates the grant with `create_grant` in the same way as `owner_start` (idempotent on `start_command_id`), with `kind = worker` (§4), classes {read, mail, drive, spawn}, scope lineage, `stations` = all paired stations at that moment (or the configured names that are paired; unknown names are skipped; the start's own station is always included), `spawn_cap`, `max_modes` {claude, codex}, `ttl_ms` = lifetime, `idle_ttl_ms` = idle. No elevation is asked (the owner's setting is the authorization). It returns the `CoordinatorSpec` with `kind: worker`, the grant id, the expiry, the idle TTL, the classes, the deck control skill, and no board-only fields. The audit row: action `coordinator.worker_grant_created`, actor `owner` / `schedule` / `issue_batch`, target the grant id, the command id, detail as P0's `audit_detail`.

## 4. The grant kind

- Deck migration V43 (`coordinator_worker_grant_schema.rs`): `ALTER TABLE tokens ADD COLUMN kind TEXT NOT NULL DEFAULT 'coordinator' CHECK (kind IN ('coordinator','worker'))`. Existing grants read `coordinator`.
- `Grant`/`NewGrant` get `kind: GrantKind { Coordinator, Worker }`. A worker grant always has exactly the classes {read, mail, drive, spawn} and scope lineage (`create_grant` refuses anything else for `kind = worker` with `invalid: a worker grant has fixed classes`).
- Proto: `CoordinatorSpec` gains `kind: GrantKind` with `#[serde(default, skip_serializing_if = "GrantKind::is_coordinator")]` (an owner grant serializes as today). `GrantView` gains `kind` (the board shows "worker").
- Rule 3 (W1): the deck's `holds_mutating_grant` (`store/coordinator.rs`) and the station's `holds_mutating` (`coordinator/bindings.rs`, used by `fleet_control.rs` `TARGET_IS_COORDINATOR`) ignore grants and bindings of kind `worker`. Every other rule-3 consequence (the target exclusions of ADR-008 §4.2, `strict_target`) uses the same predicate, so a worker-grant session is a valid fleet-control target. `holds_grant` (the `policy_now` `bound` field) still reports a worker binding as bound.
- Link scope: a worker grant adds the link scope `coordinate` to its stations like any grant (`sync_coordinate_scopes`); that is what lets its session call `coordinator.call`.

## 5. Station rules for a worker grant

- Feature `worker_grant.v1` (proto `version.rs`, offered by stations of this build). A station that negotiated it accepts a `CoordinatorSpec` with `kind: worker` from the paired deck with origin `Board` (scheduled and issue-batch starts use origin Board too; `check_spec` is unchanged for origin and import).
- `Binding` gets `kind` (from the spec); the shim row `shim_bindings` gets a `kind` column (station migration, default `coordinator`) so that a reattach keeps it.
- Fallback (goal 4): for `kind: worker`, every refusal of `tools_for`/`prepare*` that would fail the start — the handoff is not verified for the probed provider version (`handoff_unverified`), the shim bridge is not verified, a resumed start — instead starts the session with its fleet-only binding (as without a grant) and writes one info log line with the reason. The station sends nothing about the unused grant.
- The deck's `sweep_grants` revokes a worker grant that has no binding 10 minutes after its creation with the reason `never_bound` (a worker grant only; an owner grant keeps today's rules). So an unused worker grant never stays in effect and never keeps a station's `coordinate` scope.

## 6. Changing the spawn cap and the ceilings (any grant)

- `POST /api/coordinator/grants/{grant_id}/caps` (`CapsBody`) gains `spawn_cap: Option<u32>` (0–50; a worker grant 0–10) and `max_modes: Option<BTreeMap<String,String>>` (keys `claude`/`codex`, values by `valid_ceiling`; the map replaces the stored one).
- Elevation: a raise of `spawn_cap` or a higher ceiling (by `mode_within`) or a new family needs `x-fleetdeck-elevation`; a lower value or a removed family does not. `spawn_cap` below `spawn_used` is allowed (it blocks further spawns, ADR-006 §4).
- `max_modes` needs the class `spawn` or `manage` (P0 rule 3); `spawn_cap` needs `spawn`.
- The audit row `coordinator.grant_caps` holds the old and the new values. Deck only: no station message (the deck decides every spawn).
- Board: the Fleet panel's caps editor (`fleet/GrantDesktop.tsx` pattern) gets the session limit and the two ceilings for grants with `spawn`.

## 7. Tests (names are binding)

Deck:
1. `an_owner_board_start_gets_a_worker_grant_with_the_setting_values` (classes, scope, stations "all" snapshot plus the own station, cap, ceilings, lifetime, idle; the audit row with actor `owner`; the start command carries `kind: worker`).
2. `schedules_and_issue_batches_get_one_and_member_bearer_import_resume_spawn_and_cli_starts_get_none`.
3. `an_owner_coordinator_start_gets_no_extra_worker_grant`.
4. `a_station_without_worker_grant_v1_gets_no_worker_grant_and_the_start_is_unchanged`.
5. `the_setting_off_gives_no_grant_and_widening_changes_need_elevation` (each widening key; narrowing without elevation).
6. `rule_3_ignores_worker_grants_and_keeps_owner_grants` (deck `holds_mutating_grant`, `strict_target`: a fleet-control `answer_request` on a worker-grant session is accepted; on an owner-grant session refused).
7. `a_worker_grant_child_gets_no_worker_grant` (a spawn by a worker grant: no grant on the child; non-transitive).
8. `an_unbound_worker_grant_is_revoked_never_bound_after_ten_minutes_and_its_scope_goes`.
9. `the_caps_route_changes_spawn_cap_and_max_modes_with_elevation_for_a_raise`.
10. Migration V43 test (a V42 database upgrades; existing grants read `coordinator`).
Station:
11. `a_worker_spec_from_the_deck_is_accepted_and_a_coordinator_origin_or_local_start_still_refused`.
12. `an_unverified_provider_or_bridge_starts_the_session_fleet_only` (worker kind; an owner grant still refuses as today).
13. `fleet_control_may_target_a_worker_binding` (station `holds_mutating`).
14. `a_shim_reattach_keeps_the_worker_kind` (the new column).
Proto/link: 15. the feature `worker_grant.v1` and the `CoordinatorSpec`/`GrantView` serialization (an owner grant serializes as before; an N−1 station never receives `kind`).
Live (architect): one board start on a test station gets a worker grant; its Claude session lists `spawn`, `assign`, `wait_tasks`, `stop`; `fleet_view` `me` shows the grant.

## 8. Risks

- Cost: every owner start and every scheduled or issue-batch start can now start up to 2 more sessions (W3), and those cannot start more (non-transitive). The owner can turn the setting off, lower the cap, or revoke a grant on the board.
- Rule 3 is narrower: a fleet-control coordinator can now answer an approval of a session that can itself spawn. A worker grant has no fleet-control class, so it cannot answer approvals or manage other sessions.
- A worker grant gives `drive` (interrupt, stop) only over sessions that it started (lineage).
