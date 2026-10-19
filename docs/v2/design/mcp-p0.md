# MCP P0 — the board gives spawn and a mode ceiling; command_status; small fixes

- **Status:** Accepted revision 2, 2026-10-19 (adversary DESIGN RE-REVIEW PASS, report `~/fleetdeck-briefs/mcp-p0-design-r2-adversary.md`). Revision 1 (1e0a9493) got adversary DESIGN REVIEW FAIL (1 BLOCKER, 3 MAJOR, 1 MINOR; report `~/fleetdeck-briefs/mcp-p0-design-r1-adversary.md`). Revision 2 applies all five (§7 maps them).
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

A grant with the class `spawn` needs the owner's elevation in the same request, the same as a fleet-control grant or a grant with another station. Elevation and feature negotiation stay separate (R1 MAJOR 3):

- The existing block `if fleet || widened { … }` (answer and desktop caps, elevation, the `coordinator.v2` link check, the desktop features) does not change.
- Before it, a new check: `if spawns && !(fleet || widened) && !is_elevated(board, session, elevation)` → `400 elevation_required` with the message `a grant that starts sessions needs the owner's elevation`. `spawns = grant.classes.contains(&GrantClass::Spawn)`.
- So a lineage `spawn` grant on the own station needs elevation but no `coordinator.v2` link: it keeps working against a `coordinator.v1` station.

`spawn` stays outside the fleet-control classes: the kill switches do not gate it (ADR-006 behavior is unchanged).

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
- `spawnCapProblem(f)`: `null` unless `f.spawn`; else the text must match `^\d+$` and be ≤ 50 (so blank, `2.5` and `51` are problems), else the problem text `The session limit is a whole number from 0 to 50.` It is the first item in `fleetProblem`.
- `SpawnForm.tsx` (R1 MINOR): `fleetBad` becomes `coordinator && fleetShown && fleetProblem(fleet) !== null`. The grant fields are checked whenever they are shown, not only when a fleet switch is on.
- `fleetElevation` (`fleetControl.ts`): the condition `fleetOn(gate.fleet) || wide` becomes `fleetOn(gate.fleet) || wide || gate.fleet.spawn`. The label list starts with `'Starting sessions'` when `gate.fleet.spawn` is on and `fleetOn(gate.fleet)` is off; otherwise it is unchanged.
- `FleetControlFields.tsx`: a first group "Sessions it may start", shown with the rest of the fleet fields:
  - checkbox "Allow starting sessions" (`spawn`), with the help text "It can start new sessions on the grant's stations. Each start uses one unit of the session limit.";
  - number input "Session limit" (`spawnCap`), shown only when `spawn` is on;
  - two selects, shown when `spawn` or `manage` is on: "Highest Claude mode" with the options `default`, `plan`, `acceptEdits`, `dontAsk`, `auto`; "Highest Codex mode" with the eight non-bypass `policy/sandbox` values in this order: `untrusted/read-only`, `untrusted/workspace-write`, `untrusted/danger-full-access`, `on-request/read-only`, `on-request/workspace-write`, `on-request/danger-full-access`, `never/read-only`, `never/workspace-write`. Help text: "Sessions that it starts, resumes or changes never get a higher mode."

### 2.6 No coordinator spec on any other route (R1 MAJOR 2)

A `CoordinatorSpec` reaches a station only through the owner's `session.start` that `owner_start` cleaned. Two more routes carry a `SessionSpec` today:

1. **A coordinator `spawn` (and the spawn fallback of `assign_auto`).** `start()` in `crates/fleetdeck-deck/src/store/coordinator_ops.rs` gets a first check: `if spec.coordinator.is_some()` → stored rejection `Forbidden` with the message `a coordinator never starts a session with a coordinator grant` (ADR-006 §4, no transitive authority). The receipt is stored like every other step-6 rejection.
2. **The board's `session.import`.** In `crates/fleetdeck-deck/src/http/command_routes.rs`, next to the existing `Import` check of the native session ID: `if let SessionCommand::Import { spec, .. } = &req.command && spec.coordinator.is_some()` → `400 invalid_command` with the message `an import never carries a coordinator grant`. Nothing is stored or sent.

So the only `CoordinatorSpec` that a station receives is the cleaned one from `owner_start`, which never has `spawn_cap` or `max_modes`. An N−1 station never sees the new fields.

### 2.7 Out of scope for P0

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

`classes` must not be used for the grant check alone. `Tool::granted` gets one more special case: `command_status` is granted when the classes contain any of `Mail`, `Drive`, `Spawn`, `Manage`, `Answer`: the classes whose calls go through `coordinator.call` and the outbox. The desktop, capture and input tools never use the outbox and have their own status tools (`desktop_action_status`, `capture_status`), so they do not list it (architect decision during the build, 2026-10-19). The deck rule stays `is_mutating` (§3.4 item 4); the station list is a narrower filter, and the deck is the authority. The `classes: &[ToolClass::Read]` value is only for `is_own`/`is_fleet_control` (both false).

### 3.2 Mapping

- `tools::to_op`: `"command_status"` joins the read arm with `list_sessions`, `wait_task` and `wait_tasks` (it returns `Ok(None)`).
- `forward.rs` `query()`: `"command_status"` reads `command_id` (a string, else `invalid_params: \`command_id\` is required`), and returns `(CoordinatorQuery::CommandStatus { command_id: CommandId(...) }, QUERY_LIMIT)`. The exact constructor of `CommandId` is the one in `fleetdeck_proto`.

### 3.3 Tests (station)

- The catalog of a grant with only `read` has no `command_status`, and neither has an `inspect`-only grant. Grants with `mail`, `drive`, `spawn`, `manage` or `answer` (each alone) list it. An own-only binding does not list it.
- `to_op("command_status", …)` is `Ok(None)`. `query("command_status", {})` is `invalid_params`.
- A forwarded call produces `CoordinatorQuery::CommandStatus` with the given id (use the fake transport of the existing forward tests).

### 3.4 Deck: the status of a command belongs to its grant (R1 BLOCKER, R1 MAJOR 4)

Today `CommandStatus` looks up `coordinator_receipts` by `command_id` and `caller` only, and the table has no grant column. A newer grant of the same session could read the receipts and deliveries of an older (also revoked) grant. ADR-006 §6.3 says "commands attributed to this grant only". Fix:

1. **Migration V40** (new file `crates/fleetdeck-deck/src/store/coordinator_receipt_grant_schema.rs`, registered after V39 in `schema.rs` with the comment `// V40 (MCP P0): the grant of a coordinator receipt.`):
   ```sql
   ALTER TABLE coordinator_receipts ADD COLUMN grant_id TEXT;
   ```
   Forward only. Existing rows get `NULL`; they never match a status query, and the 24 h pruning removes them. To reverse: drop the column.
2. **Insert** (`crates/fleetdeck-deck/src/store/coordinator.rs`, the `INSERT INTO coordinator_receipts` of step 6): store the `grant_id` of the grant that the decision used. The worker path (`ask_coordinator` without a grant, empty grant ID) stores `NULL`.
3. **Query** (`coordinator_query.rs`, `CommandStatus`): `WHERE command_id = ?1 AND caller = ?2 AND grant_id = ?3`, with the `grant_id` of the grant that steps 1–3 of the query admitted. No row → `{"receipt": null, "deliveries": []}` as today.
4. **Class rule** (`coordinator_query.rs`): `CoordinatorQuery::CommandStatus { .. } => grant.classes.iter().any(|c| c.is_mutating())`, with `GrantClass::is_mutating` of `grants.rs`. So `read`, `inspect` and `read` + `inspect` grants are refused, the same as the station catalog filter.

## 4. Tests (deck, F1)

In the existing `owner_start` test module (or `crates/fleetdeck-deck/tests/http_commands.rs`, where the grant creation tests are):

1. Each refusal of §2.2 rules 1–5, with its exact message, and nothing is stored.
2. `spawn` without elevation → `400 elevation_required`.
3. `spawn` + `spawn_cap` 7 + `max_modes {claude: acceptEdits, codex: on-request/workspace-write}` with elevation → the stored grant has `spawn_cap` 7 and both ceilings, and the start command that goes to the station has no `spawn_cap` and no `max_modes` (serialize it and check the keys).
4. `spawn` without `spawn_cap` → the stored cap is 3.
5. `manage` with `max_modes` and without `spawn` → accepted (with elevation); `max_modes` stored.
6. `valid_ceiling`: every value of §2.2 rule 5 is valid; `bypassPermissions`, `never/danger-full-access`, `default ` (trailing space), `untrusted`, `on-request/full` and an unknown family are not.
7. §2.3: an elevated lineage `spawn` grant on the own station is accepted when the station's link has only `coordinator.v1`; the same request without elevation is `elevation_required`.
8. §2.6: a coordinator `spawn` op whose spec has a `coordinator` (with and without `spawn_cap`/`max_modes`) is a stored `forbidden` with the exact message, and no start intent is written; an `assign_auto` that would spawn with such a spec is refused the same way (if `assign_auto` can carry a spec; else not applicable). A board `session.import` with a `coordinator` is `400 invalid_command` and nothing is sent.
9. §3.4: two successive grants A and B for the same caller: B's `command_status` of A's command ID returns `receipt: null`; A's own status (while A is active) returns the receipt; another caller session gets `receipt: null`. Class rule: grants with only `read`, only `inspect`, and `read` + `inspect` are refused; each of `mail`, `drive`, `spawn`, `manage`, `answer`, `desktop`, `capture`, `input` alone is allowed.
10. The migration test of the existing schema suite passes with V40 (a V39 database with a receipt row upgrades, and the row has `grant_id` `NULL`).
11. End to end in the store: a grant made through `owner_start` with `spawn` and a Claude ceiling `acceptEdits` gets an accepted `spawn` op with mode `plan` and a refused one with mode `dontAsk` (use the existing coordinator op test helpers).

## 5. Tests (board)

In the existing `web/src/spawn` test files: `grantFields` with `spawn` on and off, with `manage` on (it sends `max_modes`); `spawnCapProblem` for `''`, `'2.5'`, `'51'`, `'0'`, `'50'`; a spawn-only grant (all fleet switches off) with a blank limit disables Start; `fleetElevation` asks for elevation for a spawn-only grant with the label `Starting sessions`.

## 6. Risks

- A spawn grant spends money and starts processes. It is bounded by the cap (≤ 50, durable `spawn_used`), the mode ceiling, the station's allowed roots, elevation at creation, and the owner's revoke.
- A ceiling such as `on-request/danger-full-access` lets a child run commands without a sandbox after approval. The owner picks it explicitly; the default is `on-request/workspace-write`.

## 7. Review map (R1 → revision 2)

| Finding | Section |
| --- | --- |
| BLOCKER — command status scoped to the caller, not the grant | §3.4 items 1–3, test 9, 10 |
| MAJOR — nested `coordinator` on spawn and import | §2.6, test 8 |
| MAJOR — deck `CommandStatus` class rule allows `inspect` | §3.4 item 4, test 9 |
| MAJOR — spawn elevation pulls in the v2 link check | §2.3, test 7 |
| MINOR — form validation only with a fleet switch on | §2.5, §5 |
