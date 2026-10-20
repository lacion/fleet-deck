# MCP P4a — the default worker grant

- **Status:** Accepted revision 3, 2026-10-20 (adversary DESIGN RE-REVIEW round 3 PASS, `~/fleetdeck-briefs/mcp-p4a-design-r3-adversary.md`). Revision 1 (f901d98e) got DESIGN REVIEW FAIL (1 BLOCKER, 7 MAJOR, 3 MINOR; `~/fleetdeck-briefs/mcp-p4a-design-r1-adversary.md`); revision 2 (61eebb2f) fixed nine and got RE-REVIEW FAIL on two (`~/fleetdeck-briefs/mcp-p4a-design-r2-adversary.md`). Revision 3 applies them (§11).
- **Program:** DL-251 (D1: every session starts with a small grant that can spawn), owner decisions W1–W3 of 2026-10-20 (`~/fleetdeck-briefs/mcp-p3-research-notes.md` §"P4 owner decisions"). P2 is folded into P4; P4b (live grant changes and the listen stream) follows.
- **Out of scope:** live changes of a running session's grant and `tools/list_changed` (P4b); a grant on resume, import, bearer-token, member or CLI starts (W2).

## 1. Goals

1. With the deck setting on (the default, W3), every eligible start (§3) gets a worker grant: classes `read`, `mail`, `drive`, `spawn`, scope `lineage`, and the stations, spawn cap, mode ceilings, lifetime and idle time of the setting.
2. Only the owner's login authorizes a worker grant: the owner's board start, an issue batch that the owner's login requests, and a scheduled run of a schedule that the owner's login last saved.
3. A worker grant never chains: a session that any grant starts gets no worker grant.
4. Worker grants are exempt from ADR-008 rule 3 (W1); owner-made coordinator grants keep rule 3.
5. A start never fails because of the worker grant, with one stated exception (§6.3).
6. The deck's work and storage for worker grants are bounded.
7. The owner can change the spawn cap and the mode ceilings of any grant after creation (deferred from P0).

## 2. The setting (deck)

`deck.toml` section `[worker_grant]` (a `DeckConfig` field `worker_grant: WorkerGrantConfig`, `RawDeck` + `RAW_DECK_KEYS`, settings registry fields, `DeckLive`):

| Key | Kind | Default | Bounds |
| --- | --- | --- | --- |
| `worker_grant.enabled` | bool | `true` (W3) | — |
| `worker_grant.stations` | list of strings | `["all"]` | `["all"]` (only as the sole entry) or 1–16 station names (R1 MINOR 1) |
| `worker_grant.spawn_cap` | number | 2 | 0–10 |
| `worker_grant.claude_mode` | enum | `acceptEdits` | `default`, `plan`, `acceptEdits`, `dontAsk`, `auto` |
| `worker_grant.codex_mode` | enum | `on-request/workspace-write` | the eight non-bypass values of P0 §2.5 |
| `worker_grant.lifetime_hours` | number | 12 | 1–168 |
| `worker_grant.idle_minutes` | number | 30 | 5–1440 |

**Widening** (`DeckHolder::danger`, elevation required), compared with the saved values: `enabled` off → on; a higher `spawn_cap`; `stations` from a list to `["all"]`, or a list with a name that was not in it; a longer `lifetime_hours` or `idle_minutes`; a ceiling change unless `mode_within(family, Some(new), old)` holds (R1 MAJOR 7: an incomparable Codex change, such as `on-request/workspace-write` → `never/read-only`, is widening). Everything else is narrowing. A change does not touch existing grants.

## 3. Eligibility and provenance (R1 BLOCKER)

A start gets a worker grant only when every condition holds: the setting is on; the command is a `SessionCommand::Start` whose spec has `coordinator: None`; the target station's current link negotiated `worker_grant.v1` and `coordinator.v1`; the in-effect worker grant count is below the deck cap (§7); and the **provenance** is one of:

| Provenance | Where | The rule |
| --- | --- | --- |
| `owner` | `http/command_routes.rs` `route` | the principal is the owner login (`Principal::Owner`), the command is not an import |
| `issue_batch` | `http/project_writes.rs` (the batch start route) | the principal of the HTTP request is the owner login; a bearer token or member principal → no grant |
| `schedule` | `scheduler/run.rs` | the schedule row's `author` is `owner` |

The provenance is a value that the route computes from the authenticated principal and passes into the attachment function; nothing in a request body or command can set it. Schedules get a new column `author TEXT NOT NULL DEFAULT 'unknown' CHECK (author IN ('owner','token','unknown'))` (deck migration, §8): the schedule create and update routes write `owner` for the owner login and `token` for a bearer token; existing rows stay `unknown` and get no grant until the owner saves them. A schedule started by the owner's "run now" click uses the schedule's `author` too.

Never: a coordinator `spawn`, `start_in_project`, `assign_auto` spawn, Fleet Git move, resume, fork, import, CLI/station-local start, member or bearer-token start, or a start that already has an owner coordinator request.

## 4. The grant kind (R1 MAJOR 1)

- Deck migration: `ALTER TABLE tokens ADD COLUMN kind TEXT NOT NULL DEFAULT 'coordinator' CHECK (kind IN ('coordinator','worker'))`. Existing grants read `coordinator`.
- `kind` is server-owned. The proto `CoordinatorSpec` gains `kind: GrantKind` (`#[serde(default, skip_serializing_if = "GrantKind::is_coordinator")]`), but `owner_start` refuses a board-supplied `kind: worker` with `400 invalid_grant` "the grant kind is set by the deck", always stores `coordinator`, and the deck builds the outgoing `kind` from the stored grant only. Only the attachment function of §5 creates `kind = worker`, always with exactly the classes {read, mail, drive, spawn} and scope lineage (`create_grant` refuses anything else for a worker).
- `GrantView` gains `kind` and `max_modes` (R1 MINOR 2; backward compatible: absent in old answers means `coordinator` and `{}`).
- Rule 3 (W1): the deck's `holds_mutating_grant` and the station's `holds_mutating` ignore grants and bindings of kind `worker`; `strict_target` and the station's `TARGET_IS_COORDINATOR` use these predicates. `holds_grant` (the `policy_now` `bound` field) still reports a worker binding as bound.

## 5. Atomic attachment and retries (R1 MAJOR 2)

- One store function `submit_start_with_worker_decision(original: Command, provenance, …)` runs in one deck write transaction: it computes the decision (`grant` or `none`, with the reason) from the setting and the conditions of §3, writes the row `worker_attach(command_id PK, original_sha256, decision, grant_id NULL, created_at)`, creates the grant when the decision is `grant` (with `start_command_id` = the command ID), and inserts the final command (the original plus the deck-built `CoordinatorSpec` for a grant) into the commands table (today's start path; no deck outbox item).
- A retry with the same command ID reads `worker_attach` first: the same `original_sha256` returns the stored command and its receipt (the stored decision is reused even when the setting, the feature or the link changed since); another `original_sha256` → `command_id_reused`. The board-supplied command is the canonical request; the final command is derived from it and the stored decision.
- No path creates a worker grant outside this transaction, so a crash leaves either nothing or the decision, the grant and the command together.

## 6. Station rules

### 6.1 Acceptance
Feature `worker_grant.v1` (proto `version.rs`, offered by stations of this build). A station that negotiated it accepts a `CoordinatorSpec` with `kind: worker` from the paired deck with origin `Board` (`check_spec`: origin, import and structure checks unchanged). `Binding` gets `kind`; `shim_bindings` gets a `kind` column (station migration, default `coordinator`) so a reattach keeps it.

### 6.2 Fallback (R1 MAJOR 3)
For `kind: worker`, `check_spec` keeps every origin, import and structure refusal, but a handoff refusal (`handoff_unverified`, the shim bridge not verified) does not refuse the command: the station removes the worker `CoordinatorSpec` from the spec before the launch. The launch then prepares the fleet-only binding only when the fleet handoff is verified (the same gate as today), else `Tools::None`. The same applies when `tools_for` would refuse (a resumed start), and when saving the shim binding fails after the credential was minted.

**The handoff outcome (R2 BLOCKER 1).** The start receipt is committed before the launch, so it cannot report the handoff. Instead, after the launch preparation (the end of `tools_for`/`prepare*`, including the shim row save), the station appends one durable station event `coordinator.worker_outcome { grant_id, start_command_id, outcome: "skipped", reason }` for every worker grant whose credential did not reach a provider (the admission skip of `check_spec` emits it too, right after the receipt). A used worker grant needs no outcome event: its `coordinator.bound` shows the use. The event is a station event like `coordinator.bound` (decodable by older decks as `EventBody::Unknown`, L16), so it is stored, acknowledged and replayed through the event stream. The snapshot does not carry it (the snapshot frame has only the binding projection); an outcome lost to a snapshot recovery leaves the grant to its idle expiry from creation (§6.4). Reasons: `handoff_unverified`, `bridge_unverified`, `resumed`, `prepare_failed`, `shim_row_failed`.

### 6.3 Queued starts and a downgraded station (R1 MAJOR 4)
The deck's outbox sends a command that carries a worker `CoordinatorSpec` only on a link that negotiated `worker_grant.v1`. On a link without it, the command waits (it is not sent, so no older station ever decodes `kind`) until a compatible link or the command's delivery TTL; at the TTL it fails as any undelivered command (`expired`) and the deck revokes its grant with the reason `undelivered`. This is the one case in which the worker grant can make a start fail (goal 5), and it needs a station downgrade while the start is queued; the owner starts again.

### 6.4 Unused grants (R1 MAJOR 5)
No `never_bound` sweep. The deck revokes a worker grant at once when it ingests `coordinator.worker_outcome` with `skipped` for it (reason `handoff_skipped`; the event must name a worker grant of that station and its start command, else it is ignored and logged) or when the start command fails or is refused (reason `start_failed`). When no outcome and no `coordinator.bound` ever arrive (a lost station), the idle time counted from creation ends the grant (30 minutes by default). A used worker grant binds through `coordinator.bound` as any grant and ends by its lifetime, its idle time (counted from creation until the first use, then from `last_used_at`), revocation, or the session's end. When the deck receives a `coordinator.bound` for a worker grant that is already terminal, it keeps it terminal and queues `session.coordinator_revoke` for that session, so the station's binding ends too.

**PRE-PR note (R1 MAJOR 1).** A start can also end before the handoff for reasons that are not handoff refusals (a failed setup command, a failed workspace preparation). The deck therefore also revokes every worker grant of a session (reason `session_ended`) when it ingests `session.ended` for that session; a grant names its session by `session_id`, or before its first binding by the receipt of its start command. This also ends a used worker grant at the session's end. Owner grants are not changed. Because `session.ended` can be folded before the start receipt is stored (R2 BLOCKER), the deck also revokes the worker grant when it stores an accepted start receipt that names a session whose summary state is already `exited` or `lost` on the grant's station.

## 7. Bounds (R1 MAJOR 6)

- At most 200 worker grants in effect deck-wide (a constant). At the cap the decision is `none` with the reason `worker_cap`, and the start goes on without a grant (one audit row `coordinator.worker_grant_skipped`).
- Retention (R2 BLOCKER 2): `sweep_grants` deletes (a) worker grants that are terminal (`revoked`, or `expired` past `expires_at`) for more than 7 days, together with their `worker_attach` rows and their lineage rows, and (b) every `worker_attach` row with `decision = none` older than 7 days, independent of any grant. Owner grants keep today's retention. Seven days is longer than every delivery TTL and the commands table keeps its own command records, so a retry of a command whose `worker_attach` row was pruned finds the retained command record: the command store's own deduplication answers it with the stored receipt (or `command_id_reused` for another payload) and never dispatches it again; a new command ID never collides with a pruned one (ULIDs).
- **PRE-PR note (R1 MAJOR 2).** A renewal cannot pass the cap: `renew_grant` refuses a worker grant while 200 worker grants are in effect, and when the renewal's `coordinator.bound` arrives at a full cap, the deck revokes the grant (reason `worker_cap`) and queues `session.coordinator_revoke` for its session.
- With at most 200 worker grants plus the owner's grants in effect, the coverage scans of `grant_scopes.rs` stay bounded; an issue batch of 50 starts creates at most 50 grants and, at cap 2, at most 100 children, which get no grant.

## 8. Changing the spawn cap and the ceilings (any grant)

- `POST /api/coordinator/grants/{grant_id}/caps` (`CapsBody`) gains `spawn_cap: Option<u32>` (0–50; a worker grant 0–10) and `max_modes: Option<BTreeMap<String,String>>` (keys `claude`, `codex`; values by `valid_ceiling`; the map replaces the stored one; `max_modes` needs the class `spawn` or `manage`, `spawn_cap` needs `spawn`).
- Narrowing (no elevation) and widening (elevation) follow §2 exactly, compared with the stored values inside the update transaction (as `store/coordinator_desktop/budget.rs` does): a lower `spawn_cap` is narrowing; for `max_modes`, the change is narrowing only when every family in the new map was in the old map and `mode_within(family, Some(new), old)` holds; any other change (a new family, a higher or incomparable ceiling) is widening. A removed family is narrowing. `spawn_cap` below `spawn_used` is allowed.
- The audit row `coordinator.grant_caps` holds the old and the new values. Deck only (the deck decides every spawn).
- Board: the Fleet panel caps editor shows the two ceilings for a grant with `spawn` or `manage` (initialized from `GrantView.max_modes`; a family without a ceiling shows "not allowed") and the session limit for a grant with `spawn`; saving one field sends only that field.

## 9. Tests (names are binding)

Deck:
1. `an_owner_board_start_gets_a_worker_grant_with_the_setting_values` (all fields; the audit row; the command carries `kind: worker`).
2. `issue_batches_by_the_owner_login_get_one_and_by_a_drive_token_get_none` (the real batch endpoint; a forged `Board` origin in the body changes nothing).
3. `a_schedule_gets_one_only_when_the_owner_login_saved_it_last` (author owner/token/unknown; "run now").
4. `members_bearer_tokens_imports_resumes_spawns_and_cli_starts_get_none`.
5. `the_grant_kind_is_server_owned` (a board-supplied `kind: worker` is refused; an owner grant is stored and sent as `coordinator`).
6. `a_retry_reuses_the_stored_decision_across_setting_and_feature_changes` (setting on→off and off→on between retries; a different original → `command_id_reused`; a crash point between the decision and the command leaves nothing).
7. `a_queued_worker_start_waits_for_a_compatible_link_and_expires_with_its_grant_revoked`.
8. `a_skipped_or_failed_start_revokes_its_worker_grant_at_once_and_a_late_bound_event_revokes_the_station_binding` (the outcome event after an admission skip, after a launch skip, and after a shim row save failure; the event replayed after a reconnect and carried by a snapshot; a forged outcome for another station's or an owner grant is ignored; no outcome at all → idle expiry from creation).
9. `the_deck_wide_cap_and_the_retention_bound_worker_grants` (201st start without a grant; a 50-start batch; pruning after 7 days; a stale retry refused; sustained no-grant automation with the setting off, then pruning of its `none` rows, then a replay refused).
10. `the_setting_and_the_caps_route_need_elevation_for_every_widening` (each widening key; incomparable Codex ceilings both directions; a new family; concurrent caps updates compared in the transaction; narrowing without elevation).
11. `rule_3_ignores_worker_grants_and_keeps_owner_grants`.
12. `fleet_control_approval_of_a_worker_spawn_stays_inside_the_worker_grant` (R1 MINOR 3: a fleet coordinator answers a worker's spawn approval; the spawn charges the worker's cap, keeps its stations and ceilings, is refused after the worker grant's revocation, and the child has no grant; the same non-transitivity through `assign_auto`, `start_in_project` and a Fleet Git move; owner-grant targets stay refused).
13. Migrations: `tokens.kind`, `schedules.author`, `worker_attach` (an upgraded database: existing grants `coordinator`, schedules `unknown`).
Station:
14. `a_worker_spec_from_the_deck_is_accepted_and_coordinator_origin_local_and_import_still_refused`.
15. `an_unverified_provider_or_bridge_skips_the_worker_grant_at_admission_and_launch` (direct and shim hosts; fleet-only only with a verified fleet handoff, else no tools; the receipt says `skipped`; a resume).
16. `fleet_control_may_target_a_worker_binding`.
17. `a_shim_reattach_keeps_the_worker_kind`.
Proto/link: 18. `worker_grant.v1` and the `CoordinatorSpec`/`GrantView` serialization (an owner grant serializes as before; `GrantView` without `kind`/`max_modes` still decodes).
Board: 19. the caps editor (ceilings for `spawn` or `manage`, readback, one-field save) and the settings section.
Live (architect): one board start on a test station gets a worker grant; its Claude session lists `spawn`, `assign`, `wait_tasks`, `stop`; `fleet_view` `me` shows the grant.

## 10. Review map (R1 → revision 2)

| Finding | Section |
| --- | --- |
| BLOCKER — bearer-token issue batches get worker grants | §1 goal 2, §3 provenance (owner login only; schedules by `author`), tests 2, 3 |
| MAJOR — `kind` not server-owned | §4, test 5 |
| MAJOR — eligibility recomputed on retries | §5 (`worker_attach`, one transaction), test 6 |
| MAJOR — fallback misses `check_spec` and promises a fleet binding | §6.2, test 15 |
| MAJOR — queued sends to a downgraded station | §6.3, test 7 |
| MAJOR — `never_bound` confuses delay with non-use | §6.4 (receipt outcome, no time sweep, late bound → revoke), test 8 |
| MAJOR — grant growth and coverage scans | §7 (cap 200, 7-day retention), test 9 |
| MAJOR — incomparable Codex ceilings | §2 and §8 widening rule, test 10 |
| MINOR — stations union in the registry | §2 (`["all"]` list form) |
| MINOR — caps editor readback and manage-only grants | §4 `GrantView.max_modes`, §8 board, test 19 |
| MINOR — authority confinement after the exemption | test 12 |

## 11. Review map (R2 → revision 3)

| Finding | Section |
| --- | --- |
| BLOCKER — the start receipt cannot report a later launch fallback | §6.2 (the durable event `coordinator.worker_outcome` after launch preparation), §6.4, test 8 |
| BLOCKER — no-grant decisions escape the retention | §7 retention (b), test 9 |
