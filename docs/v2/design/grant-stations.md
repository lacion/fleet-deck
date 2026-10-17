# Coordinator grant target stations (ADR-006 §"Creation", DL-230 item 7)

Status: proposed (architect, 2026-10-18). High risk (authorization): adversary DESIGN REVIEW before the build.

## 1. Problem

ADR-006 §"Creation" says the owner picks the grant's *target stations* on the spawn form (`tokens.stations`; default: the coordinator's own station). The code never built it:

- `CoordinatorSpec` (`crates/fleetdeck-proto/src/command.rs:57`) has no station list; `owner_start` (`crates/fleetdeck-deck/src/http/grant_routes.rs:208`) builds `NewGrant::new(station, ..)`, whose `stations` is only the coordinator's station (`store/grants.rs:111`).
- A target station that runs a coordinator op (a land, a dispatch) asks the deck `coordinator.authority_check` (station → deck, scope `coordinate`). `create_grant` gives the link scope `coordinate` only to the coordinator's own station (`store/grants.rs:333-352`), so every other station is refused (`forbidden`) and the op ends `authority_lost`.

Result: a coordinator cannot act on another station in production; the FG-10 tools (`project_branches`, `project_fetch`, `land_session`) are limited to one station.

## 2. Design

### 2.1 Wire

`CoordinatorSpec` gets one field:

```rust
/// The target stations of the grant besides the coordinator's own station
/// (ADR-006 §"Creation"). Skipped when empty, so a grant without targets
/// serializes as before (byte-compatible with coordinator.v1 stations).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
#[ts(optional, as = "Option<Vec<StationId>>")]
pub stations: Vec<StationId>,
```

The station never reads it (the deck owns the grant); the deck fills it from the board's request and echoes the stored list.

### 2.2 Deck (`owner_start`, `create_grant`)

1. `grant.stations` = the coordinator's own station ∪ `spec.stations` (a set; order is not kept).
2. Each station in `spec.stations` must be a paired station (`stations` table): else 400 `invalid_grant`, "Unknown station <id>.".
3. At most 16 stations in the set: else 400 `invalid_grant`, "A grant has at most 16 stations.".
4. A grant with a station other than its own is a fleet-control grant for the checks only: it needs the owner's elevation in the same request (400 `elevation_required`), and every station of the set must have negotiated `coordinator.v2` on its current link (the existing loop, 409 `unsupported`). A lineage grant with targets keeps its lineage drive rule (ADR-006); the targets widen only where it may act.
5. `create_grant` gives the link scope `coordinate` to **every** station of the set in the same transaction (today only the own station). Pairing alone still never grants it.
6. The `coordinate` scope of a station is removed when no `active` grant names that station any more: at a grant's revocation, expiry and idle end, in the same transaction that ends the grant (a new helper `sync_coordinate_scopes(tx)` recomputes the set from the active grants and writes `stations.scopes`). Today the scope is never removed; this closes that too. The link's scope cache follows (DL-211: the existing cache refresh after a grant change).
7. Every existing check that uses `grant.stations` (the target rules of ADR-006 §6 and ADR-008 §4.2, the FG-10 `project_branches` rule "every home of the project is on a grant station") is unchanged: they now see the wider set.

### 2.3 Board

The spawn form's "Coordinator" section gets "Target stations": a multi-select of the paired stations except the session's own station (which is always included and shown as "This station (always)"). Default: none (only this station). Text under it: "The coordinator can act on sessions of these stations. Each one needs a current link with coordinator v2." The grant view lists the stations by name.

### 2.4 Audit

The grant creation audit row gains `stations` (ids, in the detail). The revocation and expiry rows are unchanged.

## 3. Compatibility

- A board that sends no `stations` gets today's behavior.
- An older station never sees the field (skipped when empty; a grant with targets is a fleet-control grant, which already needs `coordinator.v2`).
- Stored grants keep their `stations` (today always the own station).

## 4. Tests

- Deck store: `create_grant` with targets gives `coordinate` to each target; revocation, expiry and idle end remove it when no other active grant names the station, and keep it when one does.
- Deck routes: unknown station (400), 17 stations (400), targets without elevation (400), a target without `coordinator.v2` (409), the stored and echoed list, the audit detail.
- CLI loopback (`fleet_git_coordinator.rs`): the T4.6 tests stop widening the grant in `deck.db`; they ask for the targets in the start request; `land_session` on bravo succeeds; after a revocation, bravo loses `coordinate` and a later authority check is refused.
- Board: the multi-select, the default, the texts.

## 5. Risks

- A target station gets `coordinate` while a grant names it. With `coordinate` a station can relay coordinator calls only for a session that holds a bound grant credential on that station (ADR-006 §5.4); a target station has no such session unless the owner started a coordinator there. The scope is removed when no active grant names the station.
- Widening the set widens what an `inspect` grant reads (ADR-008 §8 "Exfiltration across stations" accepted, bounded by the station list): the owner chooses the list with elevation.
