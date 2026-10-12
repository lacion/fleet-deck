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

### Round 4 (2026-10-12, from 16:01 CEST, time box end 16:20)

- Committed round 3 (dc8cc02); `git merge --no-edit v2` merged cleanly
  (69833de; lane D shell effects, lane A/C docs; no proto change, so no
  `make ts-types`).
- No review feedback on round 3 was available at the round start.
- 6-5 (still partial, DL-213): test 25b. Deck schema V10
  `tokens.renew_generation`; `DeckStore::renew_grant`; `record_lifecycle`
  and `bind_grant` accept a higher generation only when it equals the
  pending renewal (closes the round-2 follow-up "unsolicited generation
  increase"); `POST /api/coordinator/grants/{grant_id}/renew` queues
  `session.coordinator_renew`; board "Renew" button.
  - `cargo nextest run -p fleetdeck-deck -E 'test(renewal_needs_an_idle) | test(store::coordinator) | test(migrations) | binary(coordinator) | binary(deck_link)'` -> 31 passed.
  - `cargo nextest run -p fleetdeck-deck -E 'binary(board_fleet) | test(methods) | test(renewal_needs)'` -> 8 passed (first run failed on the error key: the API body uses `error`, not `code`; test fixed).
  - `bun test src/fleet` -> 6 pass; `bun run --cwd web typecheck` -> clean; `bun run --cwd web lint` -> clean (after `biome format` of the test).
- Status at the time box:
  - Done: 6-1 Workspaces, 6-3 Assign auto, 6-4 Tasks, 6-2 Mail.
  - Open: 6-5 Coordinator. Remaining work (DL-209): handoff start-path
    wiring and the §8.2 secrecy tests, the durable station coordinator
    outbox (§6.5, §8.4), snapshot projection (25e),
    the M2-peer test (26), grant creation at an owner start with the
    control skill, live smoke §8.5 (gates the capability).
- Checks after the merge: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings` (results in the
  round summary).

### Round 5 (2026-10-12, from 16:09 CEST, time box end 16:20)

- Committed round 4 (fc2c198); `git merge --no-edit v2` merged cleanly
  (ee38ccd; lane A docs and log; no proto change, so no `make ts-types`).
- Recorded the round-4 review findings below. The reviewer reported that
  its format and lint commands were blocked in its environment; the
  executor's checks of this round are in the round summary.
- 6-5 (still partial, DL-214): test 26. `coordinator.call` and
  `coordinator.query` now need `coordinator.v1` in
  `crates/fleetdeck-link/src/methods.rs` and in the ADR-004 §9.6 table
  (both builds offer the feature, so M3 peers are unchanged).
  - `cargo nextest run -p fleetdeck-link -p fleetdeck-proto -p fleetdeck-deck -p fleetdeck-station -E 'package(fleetdeck-link) & test(methods) | binary(method_table) | binary(adr_revision2) | binary(coordinator) | test(a_bound_coordinator) | test(coordinator::forward) | binary(link_service) | test(a_peer_without_the_coordinator)'` -> 26 passed (first run: 2 failures, the ADR row literal in `method_table.rs` and an ADR-order case that used a link without the feature; both tests updated).
- Status at the time box:
  - Done: 6-1 Workspaces, 6-3 Assign auto, 6-4 Tasks, 6-2 Mail.
  - Open: 6-5 Coordinator. Remaining: handoff start-path wiring and the
    §8.2 secrecy tests, the durable station coordinator outbox (§6.5,
    §8.4), snapshot projection (25e), grant creation at an owner start
    with the control skill, live smoke §8.5, and the round-4 follow-ups.

### Round 6 (2026-10-12, from 16:13 CEST, time box end 16:20)

- Committed round 5 (a3ead68); `git merge --no-edit v2` merged cleanly
  (6dcf602; lane A docs and log; no proto change, so no `make ts-types`).
- Review of round 5 (test 26): no new findings. The reviewer again
  reported that its format and lint commands were blocked in its
  environment.
- 6-5 stays open. The next slice (the coordinator spec on `session.start`,
  test 13 `a_station_local_start_with_a_coordinator_spec_is_rejected`, and
  the §8.2 handoff wiring) changes the proto command, the station decision
  and the provider start path; it does not fit in the 7 minutes before
  the time box, so this round starts no partial code.
- Status at the time box (16:20):
  - Done: 6-1 Workspaces, 6-3 Assign auto, 6-4 Tasks, 6-2 Mail.
  - Open: 6-5 Coordinator. Remaining: the coordinator spec on
    `session.start` and the handoff wiring with the §8.2 secrecy tests
    (10–13), the durable station coordinator outbox (§6.5, §8.4), snapshot
    projection (25e), grant creation at an owner start with the control
    skill, live smoke §8.5, and the review follow-ups.

### Round 7 (2026-10-12, from 16:14 CEST)

- Committed round 6 (42b27a9); `git merge --no-edit v2`: already up to date.
- Review of round 6: "continue: 6-5" with no findings on code.
- 6-5 slice (DL-215): `SessionSpec.coordinator` (`CoordinatorSpec`,
  `ControlSkill`) in crates/fleetdeck-proto/src/command.rs, `make ts-types`
  (web/src/gen/command/{CoordinatorSpec,ControlSkill,SessionSpec}.ts);
  every `SessionSpec` literal gets `coordinator: None`. Station start
  checks in crates/fleetdeck-station/src/manager/coordinator_command.rs
  (`guard` for the local socket, `check_spec` from
  crates/fleetdeck-station/src/manager/decide.rs `start`). Credential
  handoff value in crates/fleetdeck-station/src/coordinator/handoff.rs
  (`Handoff::prepare`, `Handoff::apply`).
- Tests (all passed):
  `cargo nextest run -p fleetdeck-station -E 'test(coordinator_spec) | test(a_station_local_start_with_a_coordinator_spec_is_rejected)'` -> 3 passed;
  `cargo nextest run -p fleetdeck-cli --test coordinator_secret` -> 2 passed.
- 6-5 still open: start effect wiring of `Handoff` (binding insert,
  env, args) and the deck owner start with a grant and the control skill
  (§7.1), the durable station coordinator outbox (§6.5, §8.4), snapshot
  projection 25e, live smoke §8.5.

### Round 8 (2026-10-12, from 16:25 CEST)

- Committed round 7 (8771bf4); `git merge --no-edit v2` -> 0cc328f (lane A
  docs only; no proto change, no `make ts-types`).
- Review of round 7: "wire Handoff into session start" and two coverage
  findings (test 10 storage and log scans on station and deck; test 12
  deck-side rejection); recorded in the follow-up table.
- 6-5 slice (DL-216): the start effect hands the coordinator credential to
  the child. `ProviderAdapter::start_coordinated` and `CoordinatorLaunch`
  (crates/fleetdeck-providers/src/adapter.rs, coordinator.rs), Claude
  implementation (crates/fleetdeck-providers/src/claude/driver.rs),
  `Bindings` endpoint and handoff check
  (crates/fleetdeck-station/src/coordinator/bindings.rs), link runtime sets
  the endpoint (crates/fleetdeck-station/src/link/runtime.rs),
  crates/fleetdeck-station/src/manager/coordinator_start.rs and its call
  in crates/fleetdeck-station/src/manager/target.rs; fake agent launch log
  `FLEETDECK_FAKE_LAUNCH_LOG` (crates/fleetdeck-testkit/src/fake.rs).
- Tests: `cargo nextest run -p fleetdeck-station -E 'test(coordinator_start) | test(coordinator_spec) | test(a_station_local_start_with_a_coordinator)'` -> 5 passed.
- 6-5 still open: deck owner start with a grant and the control skill
  (§7.1, board toggle), binding activation from the native identity and
  revocation at process exit, the durable station coordinator outbox
  (§6.5, §8.4), snapshot projection 25e, live smoke §8.5.

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

Review of round 4 (item 6-5, renewal 25b), recorded in round 5:

| Item | Finding | File |
|---|---|---|
| 6-5 | The renewal (`renew_grant`) and its `session.coordinator_renew` outbox item are written in two transactions, as for revoke; a crash between them leaves a pending renewal that the station never receives. Write both in one transaction (A5). | `crates/fleetdeck-deck/src/http/grant_routes.rs`, `crates/fleetdeck-deck/src/store/grants.rs` |
| 6-5 | No test covers the successful path from `POST /api/coordinator/grants/{id}/renew` through the link to the station and back to an active grant (`coordinator.bound` of the renewal generation); the HTTP test checks only 403, 409 and 404. | `crates/fleetdeck-deck/tests/board_fleet.rs` |
| 6-5 | The deck does not refuse a coordinator spec itself from the station capability; the station decision refuses it (`unsupported`, `handoff_unverified`) and the deck relays the stored receipt (DL-215). Add the deck-side capability check for an earlier board message. | `crates/fleetdeck-deck/src/http/` |
| 6-5 | §8.2 test 10 covers the station store bytes (DL-216) but not the deck store, the deck audit log or the station and deck log output (tracing) for the credential. Add a scan of the deck database and of captured logs. | `crates/fleetdeck-station/src/manager/coordinator_start_tests.rs`, `crates/fleetdeck-deck/tests/` |
| 6-5 | §8.2 test 12 asks the deck to refuse a coordinator spec `unsupported`; only the station decision refuses it (DL-215). Add the deck-side check and its test. | `crates/fleetdeck-deck/src/http/` |
| 6-5 | The Codex adapter has no `start_coordinated`; a Codex coordinator start fails `handoff_unverified` even after a live smoke (DL-216). | `crates/fleetdeck-providers/src/codex/` |
| 6-5 | Nothing calls `Bindings::native_identity` or revokes a binding at process exit yet; a coordinator binding stays `pending` (ADR-006 §5.5, §5.6). | `crates/fleetdeck-station/src/manager/target.rs` |
