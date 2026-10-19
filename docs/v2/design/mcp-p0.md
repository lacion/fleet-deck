# MCP P0 — the board gives spawn and a mode ceiling; command_status; small fixes

- **Status:** Draft revision 1, 2026-10-19. Needs an adversary DESIGN REVIEW (authorization) before the design freezes.
- **Program:** the MCP program on branch `MCP` (owner decisions D1–D3 of 2026-10-19). P0 fixes the five findings of the MCP review (artifact `01M5A0CCJ7CS4E9429R8WCZWJ5`). P4 later builds the default worker grant on the same grant fields.
- **Related:** ADR-006 §4, §6.3, §6.4; ADR-008 §3, §5.2.

## 1. Problem

| Finding | Fact (commit 58940702) |
| --- | --- |
| F1 | The board always sends the classes `read`, `mail`, `drive` plus the fleet switches, never `spawn` (`web/src/spawn/fleetControl.ts:186-195`). `CoordinatorSpec` has no spawn cap and no mode ceiling, so `owner_start` stores `NewGrant::new` defaults: `spawn_cap` 3 and an empty `max_modes`. With an empty `max_modes`, `spawn`, `start_in_project`, the spawn fallback of `assign_auto`, `resume_session` and `set_mode` are always refused (`coordinator_ops.rs:682`, `coordinator_fleet.rs:319`). |
| F2 | `control_skill.md` named `fleet_status` and `list_tasks` (no such tools) and put `read_session` under `read`. |
| F3 | The `wait_task` description said "the end of a task", but the wait also returns a task in `question`. |
| F4 | `is_fleet_op` (`coordinator_gate.rs`) did not list `Move`. |
| F5 | The outbox tells the agent to use `command_status` after a `pending` result, but no MCP tool exposes the deck query `CoordinatorQuery::CommandStatus`. |

F2, F3 and F4 are fixed by the architect in the same branch (text and one match arm). This document decides F1 and F5.

## 2. F1 — spawn class, spawn cap and mode ceilings from the board

### 2.1 Wire (`crates/fleetdeck-proto/src/coordinator_spec.rs`)

`CoordinatorSpec` gains two **board → deck only** fields, like `stations` and the desktop caps:

```rust
/// The spawn budget of a grant with the class `spawn` (ADR-006 §6.4).
/// Board → deck only: the deck reads it in `owner_start` and clears it
/// before the start command goes to a station.
#[serde(default, skip_serializing_if = "Option::is_none")]
#[ts(optional)]
pub spawn_cap: Option<u32>,
/// The most permissive native mode for each provider family (ADR-006 §4
/// `max_modes`): keys `claude` and `codex`. Board → deck only.
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
#[ts(optional, as = "Option<BTreeMap<String, String>>")]
pub max_modes: BTreeMap<String, String>,
```

The struct keeps `deny_unknown_fields`. The deck clears both fields (`take()`, `mem::take`) at the start of `owner_start`, so a station (also an N−1 station) never receives them. `web/src/gen` is regenerated with `make`'s `export_ts` command.

### 2.2 Deck rules in `owner_start` (`crates/fleetdeck-deck/src/http/grant_routes.rs`)

All checks run before the elevation check, in this order. Every refusal is `400 invalid_grant` with the exact message:

1. `spawn_cap` present and the classes have no `spawn` → `spawn_cap needs the spawn class`.
2. `spawn_cap` > 50 → `spawn_cap is 0 to 50`.
3. `max_modes` not empty and the classes have neither `spawn` nor `manage` → `max_modes needs the spawn or manage class`.
4. A key other than `claude` or `codex` → ``unknown provider family `<key>` ``.
5. A value that is not a valid ceiling for its family → ``the mode ceiling `<value>` is not valid for `<family>` ``. Valid ceilings:
   - `claude`: `default`, `plan`, `acceptEdits`, `dontAsk`, `auto`. (`bypassPermissions` is not a valid ceiling.)
   - `codex`: `<policy>/<sandbox>` with policy ∈ {`untrusted`, `on-request`, `never`} and sandbox ∈ {`read-only`, `workspace-write`, `danger-full-access`}, except `never/danger-full-access`.
6. Then: `grant.spawn_cap = spawn_cap.unwrap_or(DEFAULT_SPAWN_CAP)` (3) when the class `spawn` is present; else `spawn_cap` stays at the `NewGrant` value. `grant.max_modes = max_modes`.

A new function in `crates/fleetdeck-deck/src/store/coordinator_ops.rs`, next to `mode_within` and with the same constants:

```rust
/// Whether `ceiling` is a valid mode ceiling for `family` (MCP P0 §2.2 rule 5):
/// a known mode of the family that is not a bypass mode.
#[must_use]
pub fn valid_ceiling(family: &str, ceiling: &str) -> bool
```

It returns `mode_within(family, Some(ceiling), ceiling) && !is_bypass(family, Some(ceiling))`. Rule 4 is checked before it.

### 2.3 Elevation

A grant with the class `spawn` needs the owner's elevation in the same request, the same as a fleet-control grant or a grant with another station. In `owner_start` the condition `if fleet || widened` becomes `if fleet || widened || spawns`, with `spawns = grant.classes.contains(&GrantClass::Spawn)`. The `coordinator.v2` link check inside that block then also applies to a spawn grant. `spawn` stays outside the fleet-control classes: the kill switches do not gate it (ADR-006 behavior is unchanged).

### 2.4 Audit

The audit row `coordinator.grant_created` gets more detail: `stations=<list>` as now, plus `;spawn_cap=<n>` when the class `spawn` is present, plus `;max_modes=<family>:<ceiling>,...` when `max_modes` is not empty (keys in order).

### 2.5 Board (`web/src/spawn/`)

`FleetControl` gains four fields, and `NO_FLEET` gets these defaults:

| Field | Type | Default |
| --- | --- | --- |
| `spawn` | boolean | `false` |
| `spawnCap` | string | `'3'` |
| `claudeCeiling` | string | `'acceptEdits'` |
| `codexCeiling` | string | `'on-request/workspace-write'` |

- `grantFields`: `classes` gets `'spawn'` after `'drive'` when `f.spawn`. The result gets `spawn_cap: Number(f.spawnCap.trim())` when `f.spawn`, and `max_modes: { claude: f.claudeCeiling, codex: f.codexCeiling }` when `f.spawn || f.manage`.
- `spawnCapProblem(f)`: `null` unless `f.spawn`; else a whole number 0..50, or the text `The session limit is a whole number from 0 to 50.` It is the first item in `fleetProblem`.
- The spawn form asks for elevation when `f.spawn` is on, in the same way as for `fleetOn(f)` or other target stations.
- `FleetControlFields.tsx`: a first group "Sessions it may start", shown with the rest of the fleet fields:
  - checkbox "Allow starting sessions" (`spawn`), with the help text "It can start new sessions on the grant's stations. Each start uses one unit of the session limit.";
  - number input "Session limit" (`spawnCap`), shown only when `spawn` is on;
  - two selects, shown when `spawn` or `manage` is on: "Highest Claude mode" with the options `default`, `plan`, `acceptEdits`, `dontAsk`, `auto`; "Highest Codex mode" with the eight non-bypass `policy/sandbox` values in this order: `untrusted/read-only`, `untrusted/workspace-write`, `untrusted/danger-full-access`, `on-request/read-only`, `on-request/workspace-write`, `on-request/danger-full-access`, `never/read-only`, `never/workspace-write`. Help text: "Sessions that it starts, resumes or changes never get a higher mode."

### 2.6 Out of scope for P0

- Changing `spawn_cap` or `max_modes` after creation (the `/caps` route). This comes with P4, where the default worker grant needs it.
- The default worker grant (D1) and its non-transitivity rule (P4).

## 3. F5 — the `command_status` tool

### 3.1 Station catalog (`crates/fleetdeck-station/src/coordinator/tools.rs`)

A new entry after `wait_tasks`:

```rust
Tool {
    name: "command_status",
    classes: &[ToolClass::Read],
    description: "Read the state of a call that answered `pending`: its receipt and deliveries, by its command_id.",
    schema: command,
},
```

with `fn command() -> Value { json!({"type": "object", "required": ["command_id"], "properties": {"command_id": {"type": "string"}}}) }`.

`classes` must not be used for the grant check alone. `Tool::granted` gets one more special case: `command_status` is granted when `classes.iter().any(|c| c.is_mutating())` (the deck rule of ADR-006 §6.3: any mutating class; `read` is not needed and is not enough). `ToolClass::is_mutating` already exists in `tool_class.rs`. The `classes: &[ToolClass::Read]` value is only for `is_own`/`is_fleet_control` (both false).

### 3.2 Mapping

- `tools::to_op`: `"command_status"` joins the read arm with `list_sessions`, `wait_task` and `wait_tasks` (it returns `Ok(None)`).
- `forward.rs` `query()`: `"command_status"` reads `command_id` (a string, else `invalid_params: \`command_id\` is required`), and returns `(CoordinatorQuery::CommandStatus { command_id: CommandId(...) }, QUERY_LIMIT)`. The exact constructor of `CommandId` is the one in `fleetdeck_proto`.

### 3.3 Tests (station)

- The catalog of a grant with only `read` has no `command_status`, and neither has an `inspect`-only grant. Grants with `mail`, `drive`, `spawn`, `manage` or `answer` (each alone) list it. An own-only binding does not list it.
- `to_op("command_status", …)` is `Ok(None)`. `query("command_status", {})` is `invalid_params`.
- A forwarded call produces `CoordinatorQuery::CommandStatus` with the given id (use the fake transport of the existing forward tests).

## 4. Tests (deck, F1)

In the existing `owner_start` test module (or `crates/fleetdeck-deck/tests/http_commands.rs`, where the grant creation tests are):

1. Each refusal of §2.2 rules 1–5, with its exact message, and nothing is stored.
2. `spawn` without elevation → `400 elevation_required`.
3. `spawn` + `spawn_cap` 7 + `max_modes {claude: acceptEdits, codex: on-request/workspace-write}` with elevation → the stored grant has `spawn_cap` 7 and both ceilings, and the start command that goes to the station has no `spawn_cap` and no `max_modes` (serialize it and check the keys).
4. `spawn` without `spawn_cap` → the stored cap is 3.
5. `manage` with `max_modes` and without `spawn` → accepted (with elevation); `max_modes` stored.
6. `valid_ceiling`: every value of §2.2 rule 5 is valid; `bypassPermissions`, `never/danger-full-access`, `default ` (trailing space), `untrusted`, `on-request/full` and an unknown family are not.
7. End to end in the store: a grant made through `owner_start` with `spawn` and a Claude ceiling `acceptEdits` gets an accepted `spawn` op with mode `plan` and a refused one with mode `dontAsk` (use the existing coordinator op test helpers).

## 5. Risks

- A spawn grant spends money and starts processes. It is bounded by the cap (≤ 50, durable `spawn_used`), the mode ceiling, the station's allowed roots, elevation at creation, and the owner's revoke.
- A ceiling such as `on-request/danger-full-access` lets a child run commands without a sandbox after approval. The owner picks it explicitly; the default is `on-request/workspace-write`.
