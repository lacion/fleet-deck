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

## Review follow-ups (after M3)

(none yet)
