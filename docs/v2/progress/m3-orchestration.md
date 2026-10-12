# M3 lane B (orchestration): progress log

Worktree `fleet-deck-worktrees/m3-orchestration`, branch `m3/orchestration`.
Items, in order: 6-1 Workspaces, 6-3 Assign auto, 6-4 Tasks, 6-2 Mail,
6-5 Coordinator. Decisions: [decisions/m3-orchestration.md](../decisions/m3-orchestration.md)
(DL-200 to DL-299). The checklist lines are in [PROGRESS.md](../PROGRESS.md).

## Round log

### Round 1 (2026-10-12, 14:22 to 14:58 CEST)

- Start: tree clean; `git merge --no-edit v2` (up to date);
  `bun install --cwd web --frozen-lockfile --offline` (web/node_modules was
  missing).
- 6-1 Workspaces: done (DL-200, DL-201, DL-202). `session.start` takes a
  `workspace` request (worktree or clone); the decision plans it, the start
  effect runs it (`crates/fleetdeck-station/src/workspace/provision.rs`);
  callsigns from the v0 rotation recorded by the new `session.named` event;
  repo catalog with scrubbed origin, default branch and group
  (`[repo_groups]` in the station config); board workspace choice, batch and
  `Nx`.
- 6-3 Assign auto: done (DL-204). `POST /api/assign`, pure ranking in
  `crates/fleetdeck-deck/src/routing.rs`, `AssignBar` on the board.
- 6-4 Tasks and 6-2 Mail: done together (DL-205, DL-206). Schema V6
  (`mail`, `tasks`), `crates/fleetdeck-deck/src/store/fleet.rs`,
  `crates/fleetdeck-deck/src/http/fleet_routes.rs`, `web/src/fleet/`.
- Commands and results:
  - `make ts-types` -> regenerated `web/src/gen` (orchestration types,
    `WorkspaceRequest`, `SessionSummary.callsign/worktree`, `session.named`).
  - `cargo nextest run -p fleetdeck-station -E 'test(workspace)'` -> 9 passed.
  - `cargo nextest run -p fleetdeck-core -E 'test(explicit_repo_groups)'` -> 1 passed.
  - `cargo nextest run -p fleetdeck-deck -E 'test(routing)'` -> 3 passed.
  - `cargo nextest run -p fleetdeck-deck --test board_assign` -> 2 passed.
  - `cargo nextest run -p fleetdeck-deck -E 'test(store::fleet) | test(migrations)'` -> 3 passed.
  - `cargo nextest run -p fleetdeck-deck --test board_fleet` -> 3 passed.
  - `bun test` (spawn, assign, fleet) -> pass; `bun run --cwd web typecheck` -> clean.
  - Playwright specs written, not run (normal round): `web/e2e/spawn.spec.ts`
    (2 new), `web/e2e/assign.spec.ts` (2), `web/e2e/fleet.spec.ts` (2).
  - End of round: `cargo fmt --all --check`, `cargo clippy --workspace
    --all-targets -- -D warnings`, `bun run --cwd web lint` (results in the
    round summary).
- Lessons: new optional fields on `SessionStarted` trip clippy
  `large_enum_variant` in the event and provider-core enums, so the callsign
  has its own `session.named` event; `#[ts(optional)]` needs an `Option`
  field; the fake claude emits no event without a prompt.
- Next: 6-5 Coordinator (ADR-006 revision 4, §8 tests).

### Round 2 (2026-10-12, from 15:01 CEST)

- Start: committed round 1 (d6a663e); `git merge --no-edit v2` had
  conflicts with lane C (plans) and lane A (auto-continue): deck schema
  renumbered in merge order (plans V6, mail/tasks V7), both event kinds
  kept (`KINDS` = 32), both proto modules and TS exports kept,
  `RuntimeApp.tsx`/`SpawnForm.tsx` keep both sides; `make ts-types`, fmt,
  clippy, web lint/typecheck, proto + fleet tests and `bun test src` pass;
  merge commit d8626e7.
- Review findings of round 1 recorded in the follow-up list below.

- 6-5 Coordinator (partial, DL-207–DL-209):
  - Proto: `CoordinatorCall { caller, binding, idempotency_key, op }`,
    ops `spawn`/`send`/`interrupt`/`stop`/`assign`/`assign_auto`/
    `complete_task`, `CoordinatorQueryEnvelope`/`CoordinatorQuery`,
    `RejectCode::BindingStale` (not stored) and `SpawnCapReached` (stored),
    `GrantView`; `make ts-types` rerun.
  - Deck: schema V8 (`tokens`, `coordinator_lineage`), grants store,
    decision order §6.2, matrix §6.3, cap §6.4, intents through the outbox,
    `coordinator.query` over the link, owner grant list/revoke routes and
    board list.
  - Station: loopback MCP ingress (`crates/fleetdeck-station/src/coordinator/`):
    limits before authentication, one 401, fixed catalog, `not_bound` for a
    pending binding, `Forward` trait for the deck side, tool → op mapping.
- Commands (new tests only):
  - `cargo nextest run -p fleetdeck-deck -E 'test(store::coordinator) | binary(coordinator) | test(migrations)'` -> 23 passed (19 unit incl. lifecycle 25c/25d, 3 link, migrations).
  - `cargo nextest run -p fleetdeck-providers -E 'test(coordinator::)'` -> 2 passed (handoff arguments, capability off).
  - `cargo nextest run -p fleetdeck-proto` -> 63 passed (new event kinds `coordinator.bound`/`coordinator.ended`).
  - `cargo nextest run -p fleetdeck-station -E 'test(coordinator::)'` -> 14 passed.
  - `cargo nextest run -p fleetdeck-deck --test board_fleet -E 'test(grant)'` -> 1 passed.
  - `cargo nextest run -p fleetdeck-deck -p fleetdeck-proto -E 'test(method) | test(route) | package(fleetdeck-proto)'` -> 70 passed.
  - `bun test src/fleet` -> 5 passed; `bun run --cwd web typecheck` -> clean.
  - Playwright `web/e2e/fleet.spec.ts` grant test written, not run (normal round).
- Open in 6-5 (stays unchecked): see DL-209.
- Lessons: the deck test module of a `#[path]` file needs `crate::store::...`
  imports, not `super::...`; a `deny_unknown_fields` op makes a tool
  argument that names another caller an `invalid_params` refusal.

### Round 3

- Committed round 2 (0a647eb); merged v2 (e7956cd): lane C's
  `file_touches` keeps schema V8, the coordinator grants moved to V9;
  `read_model.rs` runs both `record_lifecycle` and `record_touches`; proto
  `mutation.rs`/`ts.rs` keep the shell types and the coordinator query;
  `UserInput` gained `images` (now built with `UserInput::new`). Ran
  `bun install --cwd web --frozen-lockfile --offline` (lane D added two
  packages), `make ts-types`.
- Recorded the round-2 review findings below.
- 6-5 (still partial): the station forwards coordinator tool calls over
  its deck link (DL-210). `fleetdeck-link` keeps the peer's `hello`
  incarnation on `OpenLink`/`LiveLink` (`peer_incarnation`);
  `crates/fleetdeck-station/src/coordinator/forward.rs` `LinkForward`;
  `StationLink.coordinator` follows the current link.
- Commands (new tests only):
  - `cargo nextest run -p fleetdeck-station -p fleetdeck-link -E 'test(coordinator::forward) | binary(link_service) | (package(fleetdeck-link) & test(hello))'` -> 18 passed.
  - `cargo nextest run -p fleetdeck-deck --test coordinator` -> 4 passed.
  - `cargo nextest run -p fleetdeck-deck -p fleetdeck-proto -E 'test(store::coordinator) | binary(coordinator) | test(migrations) | package(fleetdeck-proto)'` -> 91 passed (after the merge).

- 6-5 (still partial, DL-211): `StationNode` starts the coordinator
  endpoint; `DeckNode::create_grant` adds `coordinate` to the caller's
  station and refreshes the link grant cache; lineage rows match the
  session id of the start receipt.
  - `cargo nextest run -p fleetdeck-deck --test deck_link -E 'test(a_bound_coordinator)'` -> 1 passed.
  - `cargo nextest run -p fleetdeck-deck -E 'test(a_spawned_session_joins_the_lineage) | test(store::coordinator)'` -> 20 passed.

- 6-5: ADR-006 §8.3 tests 25 and 25a added to
  `crates/fleetdeck-deck/src/store/coordinator_lifecycle_tests.rs` (a ping
  and a tool discovery stay at the station ingress and never reach the
  deck, so they cannot refresh the idle time).
  - `cargo nextest run -p fleetdeck-deck -E 'test(queries_return_only_targets_inside_the_grant) | test(a_successful_query_and_an_accepted_mutation)'` -> 2 passed.
- Checks at 15:53: `cargo fmt --all --check` -> ok;
  `cargo clippy --workspace --all-targets -- -D warnings` -> clean.

- 6-5 (DL-212): `session.coordinator_renew` / `session.coordinator_revoke`
  (feature `coordinator.v1`), station decision
  (`manager/coordinator_command.rs`), the owner's revoke queues the
  station command; web `METHODS` map extended; `make ts-types` rerun.
  - `cargo nextest run -p fleetdeck-station -E 'test(coordinator_command)'` -> 3 passed.
  - `cargo nextest run -p fleetdeck-link -p fleetdeck-proto -p fleetdeck-deck -E 'test(coordinator_renew_and_revoke) | binary(adr_revision2) | binary(method_table) | test(methods) | package(fleetdeck-proto)'` -> 77 passed.
  - `cargo nextest run -p fleetdeck-deck -p fleetdeck-station -E 'binary(board_fleet) | binary(coordinator) | test(a_bound_coordinator) | binary(link_service) | test(coordinator::)'` -> all passed after the order fix in `a_grant_removed_in_the_store_refuses_a_call_in_the_decision_transaction` (the grant is created before the scope is removed, DL-211).
  - `bun run --cwd web typecheck`, `lint` -> clean; `bun test src` -> 137 pass.

## Review follow-ups (after M3)

Review of round 1 (items 6-1, 6-3, 6-4, 6-2), recorded in round 2:

| Item | Finding | File |
|---|---|---|
| 6-1 | Clone URLs that carry credentials need a stricter check (also in query strings and other URL forms). | `crates/fleetdeck-station/src/workspace/provision.rs` |
| 6-1 | The start effect does not recheck the worktree/clone path (allowed root, symlinks, absence) at dispatch time, only in the decision. | `crates/fleetdeck-station/src/workspace/provision.rs` |
| 6-3 | A reused `command_id` with a different body returns the stored command instead of a conflict. | `crates/fleetdeck-deck/src/http/assign_routes.rs` |
| 6-2/6-4 | A reused `mail_id`/`task_id` with a different body returns the stored row instead of a conflict. | `crates/fleetdeck-deck/src/store/fleet.rs` |
| 6-3 | The existing-session pick does not check station capacity or provider availability (only the spawn hint does). | `crates/fleetdeck-deck/src/routing.rs` |
| 6-3 | "Spawn on <station>" opens the spawn form but drops the chosen station. | `web/src/App.tsx` |
| 6-3 | An assignment whose command was rejected shows as success. | `web/src/assign/assign.ts` |
| 6-2 | Mail targets only one session: no repo-group, station or broadcast targets. | `web/src/fleet/FleetPanel.tsx` |

Review of round 2 (item 6-5, partial), recorded in round 3:

| Item | Finding | File |
|---|---|---|
| 6-5 | `bind_grant`/`record_lifecycle` accept any higher generation that the station reports, without a matching renew request (unsolicited generation increase). | `crates/fleetdeck-deck/src/store/grants.rs` |
| 6-5 | `wait_task` filters tasks by station, not by the grant that created them or the assignee; `command_status` filters by caller, not by grant. | `crates/fleetdeck-deck/src/store/coordinator_query.rs` |
| 6-5 | Coordinator decisions write no audit rows; the routing context (sessions, infos, buried mail) is read before, not inside, the decision transaction. | `crates/fleetdeck-deck/src/store/coordinator.rs` |
| 6-5 | The lineage row of a spawn never gets the started session id, so `interrupt`/`stop` of a spawned child cannot match. | `crates/fleetdeck-deck/src/store/coordinator_ops.rs` |
| 6-5 | The grant-bearing spawn test asserts acceptance (the `grant` key is dropped at decode) instead of a rejection. | `crates/fleetdeck-deck/src/store/coordinator_tests.rs` |
| 6-5 | The tool catalog misses ADR-006 tools and renames some (`send_mail` vs `send`, etc.). | `crates/fleetdeck-station/src/coordinator/tools.rs` |
| 6-5 | Ingress tests set binding expiry by hand and never open more connections than the configured limit. | `crates/fleetdeck-station/src/coordinator/ingress_tests.rs` |
