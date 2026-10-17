# Coordinator grant target stations (ADR-006 §"Creation", DL-230 item 7)

Status: proposed, revision 2 (architect, 2026-10-18): answers the DESIGN REVIEW round 1 (`adv-grant-stations-design-r1-report.md`: B1, M1-M4, U1). High risk (authorization).

## 1. Problem

ADR-006 §"Creation" says the owner picks the grant's *target stations* on the spawn form (`tokens.stations`; default: the coordinator's own station). The code never built it:

- `CoordinatorSpec` (`crates/fleetdeck-proto/src/command.rs:57`) has no station list; `owner_start` (`crates/fleetdeck-deck/src/http/grant_routes.rs:208`) builds `NewGrant::new(station, ..)`, whose `stations` is only the coordinator's station (`store/grants.rs:111`).
- A target station that runs a coordinator op (a land, a dispatch) asks the deck `coordinator.authority_check` (station → deck, scope `coordinate`). `create_grant` gives the link scope `coordinate` only to the coordinator's own station (`store/grants.rs:333-352`), so every other station is refused (`forbidden`) and the op ends `authority_lost`.

Result: a coordinator cannot act on another station in production; the FG-10 tools (`project_branches`, `project_fetch`, `land_session`) are limited to one station.

## 2. Design

### 2.1 Wire (revision 2, M3)

`CoordinatorSpec` gets one field, used only between the board and the deck:

```rust
/// The target stations that the owner picked (ADR-006 §"Creation"), besides
/// the coordinator's own station. Board → deck only: the deck reads it in
/// `owner_start` and clears it before the start command goes to a station,
/// so a station never sees it (both coordinator.v1 and the current v2
/// decoders refuse unknown fields).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
#[ts(optional, as = "Option<Vec<StationId>>")]
pub stations: Vec<StationId>,
```

- `owner_start` takes `std::mem::take(&mut spec.stations)` before it creates the grant; the command that the deck forwards has an empty list, so it serializes byte-for-byte as today (a test decodes the forwarded command with the unchanged v1 and v2 station decoders).
- The full stored list is returned by `GrantView.stations` (the grant routes, already exists), never by a station-bound command.

### 2.2 Effective activity (revision 2, B1)

A grant is **in effect** when `state = 'active'` and `now <= expires_at` and not idle-expired (`last_used_at` is NULL or `now - last_used_at <= idle_ttl_ms`). One store function `in_effect(grant, now) -> bool` is the only definition; every check that today tests `state` and `expires_at` separately calls it.

### 2.3 The `coordinate` scope is derived (revision 2, B1, M2, M4)

- `coordinate` in `stations.scopes` is derived only from grants: a station has it exactly when at least one grant **in effect** names it in `grants.stations` (the coordinator's own station is always in that list). No other writer may add or keep it.
- One helper `sync_coordinate_scopes(tx, now) -> BTreeSet<StationId>` recomputes the derived scope for every station in the same transaction and returns the stations whose scopes changed. It keeps every other scope of the station unchanged.
- It runs, in the same transaction, after every write that can change the result:
  1. grant creation (`create_grant`);
  2. revocation (`grant_routes.rs` revoke);
  3. the lazy idle expiry of `store/coordinator.rs` and `store/coordinator_query.rs` (they already write `state = 'expired'`);
  4. the activation of a renewal in `apply_bound` (also reached by the snapshot projection): a renewal that reactivates the only grant of a station gives `coordinate` back;
  5. every lifecycle end of a grant (unbind, session end) that changes `state`;
  6. **pairing writes**: `pin_station` for an already paired key replaces the scopes with the pairing scopes and then runs the helper, so an active grant keeps its `coordinate`;
  7. **a periodic sweep**: a deck task every 30 s marks every grant that is `active` but past `expires_at` or idle-expired as `expired` (`revoked_reason = 'expired'` / `'idle'`) and runs the helper;
  8. **deck start**: `DeckStore::open` runs the sweep and the helper before the deck accepts any link (this removes stale scopes left by older code, also for a station that is offline).

### 2.4 Link scope cache (revision 2, M1)

- After any transaction of §2.3 that returned changed stations, the caller refreshes the link cache (`DeckPeers`) for **each** changed station.
- `DeckPeers::refresh` is serialized: it holds one async mutex across the database read and the publish, so two concurrent refreshes cannot publish out of order; a refresh always reads the current stored scopes (the store is the truth, the cache only follows).
- At deck start the cache is filled from the stored scopes after the §2.3 item 8 reconciliation.
- Existing refresh call sites (`grant_routes.rs:278`, `link/runtime.rs:328`) use the same serialized refresh.

### 2.5 Deck (`owner_start`, `create_grant`)

1. `grant.stations` = the coordinator's own station ∪ the taken `spec.stations` (a set).
2. Each station in `spec.stations` must be a paired station: else 400 `invalid_grant`, "Unknown station <id>.".
3. At most 16 stations in the set: else 400 `invalid_grant`, "A grant has at most 16 stations.".
4. A grant with a station other than its own is a fleet-control grant for the checks only: it needs the owner's elevation in the same request (400 `elevation_required`), and every station of the set must have negotiated `coordinator.v2` on its current link (the existing loop, 409 `unsupported`). A lineage grant with targets keeps its lineage drive rule (ADR-006); the targets widen only where it may act.
5. Every existing check that uses `grant.stations` (the target rules of ADR-006 §6 and ADR-008 §4.2) is unchanged: it now sees the wider set.

### 2.6 Board

The spawn form's "Coordinator" section gets "Target stations": a multi-select of the paired stations except the session's own station (which is always included and shown as "This station (always)"). Default: none (only this station). Text under it: "The coordinator can act on sessions of these stations. Each one needs a current link with coordinator v2." The grant view lists the stations by name.

### 2.7 Audit

The grant creation audit row gains `stations` (ids, in the detail). The revocation and expiry rows are unchanged; the sweep writes one `coordinator.grant_expired` audit row per grant it ends (actor `deck`).

## 3. Compatibility

- A board that sends no `stations` gets today's behavior.
- A station never sees the field (the deck clears it before forwarding, §2.1); the forwarded command is byte-for-byte as today.
- Stored grants keep their `stations` (today always the own station).

## 4. Tests

- Deck store: `create_grant` with targets gives `coordinate` to each target; revocation, expiry and idle end remove it when no other active grant names the station, and keep it when one does.
- Deck routes: unknown station (400), 17 stations (400), targets without elevation (400), a target without `coordinator.v2` (409), the stored and echoed list, the audit detail.
- Deck store: `sync_coordinate_scopes` for create, revoke, lazy idle expiry, renewal activation (with and without another grant), the sweep, a re-pairing during an active multi-station grant, and deck start with a stale scope and an offline station.
- Deck link: a new target station is admitted for `coordinator.authority_check` right after creation (no restart); concurrent create and revoke publish the stored result.
- Compatibility: the forwarded start command decodes with the unchanged v1 and v2 station decoders.
- FG-10 dependency (U1): the FG-10 tools and `crates/fleetdeck-cli/tests/fleet_git_coordinator.rs` are on the `fleet-git` branch, not on `v2`. This change lands on `v2` first; when `v2` merges into `fleet-git`, the T4.6 tests stop widening the grant in `deck.db` and ask for the targets in the start request.
- Board: the multi-select, the default, the texts.

## 5. Risks

- A target station gets `coordinate` while a grant in effect names it. Corrected in revision 2: `coordinate` also lets the worker sessions of that station use the worker calls that skip the grant check (`store/coordinator.rs:215`, `ask_coordinator` in `coordinator_fleet.rs:251`: a worker asks its coordinator, which updates a task and queues mail). That is the purpose for a spawned child on a target station; it gives no drive authority. Coordinator calls still need a session with a bound grant credential (ADR-006 §5.4).
- Widening the set widens what an `inspect` grant reads (ADR-008 §8 "Exfiltration across stations" accepted, bounded by the station list): the owner chooses the list with elevation.
