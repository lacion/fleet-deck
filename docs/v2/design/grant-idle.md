# A running turn keeps a grant in use, and a refused call says why (G6)

- **Status:** Draft for the architect, revision 1 (2026-10-24). Risk: **high** (an authorization change, §5). The adversary does a DESIGN REVIEW before the design is frozen and a PRE-PR REVIEW before the merge.
- **Backlog:** `docs/v2/BACKLOG.md` item B24, G6 (lines 306-308).
- **Owner decision (2026-10-24, binding):** a running turn of the coordinator session counts as use of its grant. While the coordinator session works (a turn runs), its grant does not go idle. The idle clock (`idle_ttl_ms`, 30 min today) starts when the session stops working. The absolute lifetime (`expires_at`) stays the hard cap. Independent of that: when a grant expired (idle or lifetime), the refused calls of the coordinator say so clearly, for example "Your fleet grant expired (idle for 30 minutes). The owner can renew it on the board."
- **Code base:** branch `v2`, HEAD `c4fbdbfd`. All file:line references are on this commit.
- **Evidence:** the code only. No session was started and no live database was read.

## 1. Verified facts

### 1.1 The idle rule today

1. A grant is in effect when it is `active`, `now <= expires_at`, and it is not idle: `idle_since()` is unset or at most `idle_ttl_ms` ago (`crates/fleetdeck-deck/src/store/grant_scopes.rs:38-48`, `in_effect`).
2. `Grant::idle_since()` (`crates/fleetdeck-deck/src/store/grants.rs:248-257`): a coordinator grant uses `last_used_at`; a worker grant (MCP P4a §6.4) uses `last_used_at`, else `created_at`. So an unbound coordinator grant never idles, and an unbound worker grant idles from its creation (the cleanup of a lost handoff, `docs/v2/design/mcp-p4a.md` line 74).
3. The binding sets `last_used_at` to the bind time: `apply_bound` (`crates/fleetdeck-deck/src/store/grants_lifecycle.rs:203-221`) and the renew acknowledgment (`grants.rs:484-496`). So **every bound grant has `last_used_at`**.
4. `last_used_at` moves today only for:
   - an accepted new `coordinator.call` (`crates/fleetdeck-deck/src/store/coordinator.rs:305-311`; module rule 6 at `coordinator.rs:23-25`);
   - a successful `coordinator.query` (`crates/fleetdeck-deck/src/store/coordinator_query.rs:154-160`, `touch`);
   - a desktop operation (`crates/fleetdeck-deck/src/store/coordinator_desktop/admit.rs:280`).
   ADR-006 §5.3 states this rule (`docs/v2/adr/ADR-006-coordinator-mcp.md:84`).
5. Defaults and bounds: `DEFAULT_IDLE_TTL_MS = 30 min`, `MAX_IDLE_TTL_MS = 24 h` (`grants.rs:42-44`). The column allows 1 ms to 24 h (`crates/fleetdeck-deck/src/store/schema.rs:253`). The board always sends `idle_ttl_ms: 0` (the default) (`web/src/spawn/SpawnForm.tsx:419`, `web/src/fleet/fleet.ts:316`). Worker grants take 5-1440 min (`mcp-p4a.md` line 29).

### 1.2 Where a grant ends for idle time

1. **The sweep.** `DeckStore::sweep_grants` (`grant_scopes.rs:209-237`) runs `end_stale` (`grant_scopes.rs:103-127`): lifetime first (`expired`), then idle (`idle`). It rebuilds the derived scopes, writes one `coordinator.grant_expired` audit row per ended grant, and notes the changed stations. It runs at deck start (`crates/fleetdeck-deck/src/store/mod.rs:435-440`) and every 30 s (`GRANT_SWEEP_EVERY`, `crates/fleetdeck-deck/src/serve.rs:59`, loop `serve.rs:65-82`). `sweep_grants` reads the clock before its database call (`grant_scopes.rs:217`).
2. **The lazy checks** (a call or a query finds an idle grant and expires it):
   - `decide_coordinator` (`coordinator.rs:246-254`, through `expire_idle`, `grant_scopes.rs:129-147`);
   - `coordinator_query` `check` (`coordinator_query.rs:134-150`, its own inline expiry; it reads `last_used_at`, not `idle_since()`);
   - the desktop admission and list (`crates/fleetdeck-deck/src/store/coordinator_desktop/admit.rs:118-127`, `coordinator_desktop/list.rs:53-59`).
3. Other readers of the rule use `in_effect` without a write: `station_covered` and `sync_coordinate_scopes` (`grant_scopes.rs:61-101`), `fleet_view_build.rs:276`, `display_directives_coordinator.rs:246`, `grants_lifecycle.rs:172`, `owner_grants.rs:326`, `worker_grant.rs:405`, `coordinator_desktop/authorize.rs:491`.
4. The board's Renew (`crates/fleetdeck-deck/src/http/grant_routes.rs:194-230`) renews only an `expired`/`idle` grant inside its lifetime that was bound once (`grants.rs:500-519`, `renew_grant`).

### 1.3 What the deck knows about a running turn

1. The deck read model keeps one `SessionSummary` JSON per session in `sessions(station_id, session_id, summary, updated_at)` (`crates/fleetdeck-deck/src/store/schema.rs:47-53`). `apply_events` folds each event into it in one transaction per batch (`crates/fleetdeck-deck/src/store/read_model.rs:175-276`; the loop at `read_model.rs:214-245`; `set_cursor` at `:249`).
2. `SessionSummary.current_run` (`crates/fleetdeck-proto/src/session.rs:102`) is set by `run.started` and cleared by `run.completed` or by a process end (`crates/fleetdeck-proto/src/fold.rs:91-108`, `clear_running` at `fold.rs:383-390`). `Activity::Working` means "a run is active"; `Activity::Background` means "the run ended but background work still runs" (`session.rs:18-33`; `derive_activity` at `fold.rs:466-478`). The board shows both as the Working column (`crates/fleetdeck-proto/src/status.rs:48`).
3. Reachability is memory only: `reachable: Mutex<BTreeMap<StationId, i64>>` (`store/mod.rs:262`), the value is the start of the continuous link (`crates/fleetdeck-deck/src/store/station_link.rs:20-52`, `set_reachable`, `is_reachable`, `linked_since`). Only `link::serve` and a station removal write it (`crates/fleetdeck-deck/src/link/mod.rs:140`, `:179`; `crates/fleetdeck-deck/src/peers.rs:200`).
4. A link does not always start with a snapshot. The deck asks `events.replay` with its cursor; the station answers with the events after the cursor, or with a snapshot when the incarnation differs or the cursor is too old (`link/mod.rs:1-17`, `:225-229`). No frame says "the replay is complete". The `SnapshotMark` (`store/mod.rs:305-315`) exists only after a snapshot, so it cannot show freshness after a plain replay.
5. Each link asks `station.info` at once and then every 60 s (`crates/fleetdeck-deck/src/link/info.rs:17`, `:24-59`; called at `link/mod.rs:216`). `StationInfo.head` is "the committed head `station_seq`" and `StationInfo.incarnation` is the station incarnation (`crates/fleetdeck-proto/src/link.rs:241-248`). The deck keeps the newest answer with its deck time: `infos: Mutex<BTreeMap<StationId, (StationInfo, i64)>>` (`store/mod.rs:265`, `set_station_info` at `store/mod.rs:618-633`, `station_info` at `:680-687`). The map is not cleared when a link ends.
6. The read model stores its station incarnation and cursor in `stations.incarnation_id` and `stations.cursor` (`schema.rs:42-43`; `StationRecord`, `crates/fleetdeck-deck/src/store/stations.rs:60-64`).

### 1.4 Where the refused text comes from

1. The link checks the scope of every inbound request in its read loop, before the deck sees it: `methods::authorize` (`crates/fleetdeck-link/src/methods.rs:79-124`) with the scopes of `GrantSource::scopes` (`crates/fleetdeck-link/src/control.rs:711-739`; the refusal is `Err(e) => Err(Outcome::Err(e))` at `control.rs:729`). The text is "`` `{name}` needs scope {scope}, which the pairing did not grant ``" with code `forbidden` (`methods.rs:107-116`). It is the only `forbidden` of `authorize`.
2. `coordinator.query` and `coordinator.call` need the scope `coordinate` (`crates/fleetdeck-link/src/methods_table_tail.rs:107-120`).
3. The deck derives `coordinate` of a station from the grants in effect (`sync_coordinate_scopes`, `grant_scopes.rs:69-101`; the coordinator's own station is always in `grant.stations`, `docs/v2/design/grant-stations.md` line 40). When the last grant in effect of a station ends, the station loses `coordinate` and the link cache follows (`DeckPeers::refresh_changed`, `peers.rs:123-145`; `DeckPeers` is the `GrantSource` of the deck links, `peers.rs:174-178`). From then on, every `coordinator.*` request of that station gets the link text of item 1. This is the text of the backlog report.
4. The store texts of the same situation, which a call sees only while the station still has `coordinate`:
   - `decide_coordinator`: "the station is not paired or has no `coordinate` grant" (`coordinator.rs:215-219`), `NOT_COVERED` = "No coordinator grant covers this station now." (`coordinator.rs:222-223`, `grant_scopes.rs:36`), "the coordinator grant is revoked or expired" (`coordinator.rs:234-238`), "the coordinator grant expired after its idle time" (`coordinator.rs:246-254`);
   - `coordinator_query::check`: "no `coordinate` grant", `NOT_COVERED`, "the grant is revoked or expired", "the grant expired after its idle time" (`coordinator_query.rs:93-152`).
5. The station turns the answers into MCP tool results:
   - a query and a call without the outbox: `Outcome::Err(e)` becomes the tool error `"{code}: {message}"` (`crates/fleetdeck-station/src/coordinator/forward.rs:393-410`, line 408); `jsonrpc::call` passes it on as `CallOutcome::Error` (`crates/fleetdeck-station/src/coordinator/jsonrpc.rs:87-104`), and `tool_error` makes the result (`crates/fleetdeck-station/src/coordinator/mcp_server.rs:132-135`);
   - a refused query of the store comes back as the value `{"reject": {...}}` (`crates/fleetdeck-deck/src/link/requests.rs:150`); the session reads the message in it;
   - a refused call of the store comes back as a receipt with a reject; the station shows `"{code}: {message}"` (`forward.rs:346-360`, `rejection`);
   - a mutating call goes through the durable outbox. A link refusal (`Outcome::Err`) keeps the row `pending` and the flush sends it again until `retry_until` (`crates/fleetdeck-station/src/coordinator/outbox.rs:262-281`). The tool answers `{command_id, state}`, and the session sees the refusal when it calls `command_status` (a query).
   - the desktop tools turn every link error into the fixed P6 code `desktop_unsupported` (`crates/fleetdeck-station/src/coordinator/desktop_forward.rs:407`).

### 1.5 Corrections to the backlog and the task text

1. The backlog case "its own long workflow only waits" is probably `Activity::Background`, not a running turn: a Claude Code workflow runs as a background task, and the turn that started it can end (§1.3 item 2). The binding owner decision names a running turn. This draft follows the decision and asks the owner (§8 Q1).
2. The link text is not produced because "the deck drops the derived coordinate scope when no grant covers them" alone: it is produced only when **no** grant in effect names the calling station. While another grant covers the station, the store texts of §1.4 item 4 apply.

## 2. Answers

1. **What is "a running turn" on the deck?** The deck read model has the session's summary with `current_run` set, the session state is not `exited` or `lost`, and the read model of that station is fresh (§3.2).
2. **How is the extension recorded?** As use: the deck moves `last_used_at` to the deck time when it sees the turn run (§3.3). Every check of the idle rule then works without a change, also after a deck restart.
3. **Can the coordinator fake it?** No session can write the deck read model or `last_used_at`. The station writes the run state from the provider process. The coordinator can keep a real turn running; the owner decision counts that as use, and `expires_at` stays the cap (§5).
4. **Can it extend past `expires_at`?** No. A touch needs `now <= expires_at`, and the lifetime check comes first in every check.
5. **Which grants?** Every **bound** grant: the spawn-time coordinator grant, the owner-given grant (MCP P4b) and the worker grant (MCP P4a). An unbound grant gets no extension (§3.4).
6. **Where does the clear text reach the session?** At the link refusal (new, §3.6) and at every store refusal of a caller with a grant (§3.5).

## 3. Design

### 3.1 Semantics

- A grant is **used by a turn** at deck time `now` when all of these are true:
  1. the grant is in effect at `now` (`in_effect`, unchanged), and it is bound (`binding.is_some()`);
  2. the station of the grant (`grant.station_id`) is fresh (§3.2);
  3. the read model of that station has the summary of `grant.session_id`, it parses, and `turn_running(summary)` is true:
     ```rust
     /// A running turn (G6 §3.1): a run is active and the process lives.
     #[must_use]
     pub(crate) fn turn_running(summary: &SessionSummary) -> bool {
         summary.current_run.is_some()
             && !matches!(summary.state, SessionState::Exited | SessionState::Lost)
     }
     ```
- An open approval or question inside a run (`Activity::NeedsYou`) is a running turn: the run is active.
- Background work without a run (`Activity::Background`) is not a running turn (§1.5, §8 Q1).
- The deck records a use by a turn as `last_used_at = MAX(last_used_at, now)`. So the idle clock starts at the last moment the deck saw the turn run. That moment is at most one sweep period (30 s) before the real end of the turn, or the last event batch of the turn. The clock never starts later than the real end (fail safe).
- A touch never revives a grant: condition 1 is false for a grant that is already idle past its TTL, past its lifetime, revoked or expired. The owner's Renew stays the only way back.

### 3.2 Fresh read model (fail closed)

A station is **fresh** when all of these are true:

1. the link is up: `linked_since(station) == Some(t_link)`;
2. the newest `station.info` answer came on this link: `station_info(station) == Some((info, info_at))` with `info_at >= t_link`;
3. in the transaction of the touch, the read model has caught up with that answer: `stations.incarnation_id == info.incarnation` and `stations.cursor >= info.head`.

Items 1 and 2 are memory, read before the database call. Item 3 is read in the transaction. When the deck cannot show all three, there is no touch. Cases:

| Case | Result |
| --- | --- |
| Link down, deck restart before the station links again | No touch (item 1). The start sweep ends a grant that is idle past its TTL. |
| Link up, `station.info` not answered yet, or the answer is from an earlier link | No touch (item 2). |
| Replay or snapshot not applied up to the reported head | No touch (item 3). |
| The station restarted with a new incarnation and the snapshot is not applied | No touch (item 3, incarnation). |
| A summary that does not parse | No touch. |

A fresh station that is stuck (link alive, no events) keeps its last state. This is the trust that the board already gives the station link; `expires_at` caps it (§5 T3).

### 3.3 Deck store: the touch (`store/grant_turns.rs`, new)

A new module `crates/fleetdeck-deck/src/store/grant_turns.rs`, declared in `grant_scopes.rs` as a child module (the `#[path]` pattern of `grants.rs:27-34`):

```rust
#[path = "grant_turns.rs"]
pub(crate) mod turns;
```

Its content:

```rust
//! A running turn is use of its grant (G6, design `grant-idle.md` §3).
//!
//! The deck moves `last_used_at` of a bound grant in effect to the deck
//! time while the read model shows a running turn of the grant's session
//! and the station is fresh (§3.2). The sweep does it every 30 s and the
//! event fold does it for each batch. Every idle check reads
//! `last_used_at`, so it needs no change.

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::SessionSummary;
use fleetdeck_proto::{IncarnationId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::in_effect;
use crate::store::grants::{GRANT_COLUMNS, Grant, finish, grant_row};
use crate::store::{DeckStore, Result};

/// The incarnation and head of the `station.info` answer of the current
/// link of a station (§3.2 items 1 and 2).
pub(crate) type Stream = (IncarnationId, u64);

/// `turn_running` exactly as in §3.1.

/// True when the read model of `station` has the incarnation of `stream`
/// and its cursor is at least the head (§3.2 item 3).
fn caught_up(tx: &Connection, station: &StationId, stream: &Stream) -> Result<bool> {
    let row: Option<(Option<String>, i64)> = tx
        .query_row(
            "SELECT incarnation_id, cursor FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.is_some_and(|(inc, cursor)| {
        inc.as_deref() == Some(stream.0.as_str())
            && u64::try_from(cursor).is_ok_and(|c| c >= stream.1)
    }))
}

/// Touches `grant` when §3.1 holds; returns true when it wrote.
fn touch_if_running(tx: &Connection, grant: &Grant, stream: &Stream, now: i64) -> Result<bool> {
    if grant.binding.is_none() || !in_effect(grant, now) {
        return Ok(false);
    }
    if !caught_up(tx, &grant.station_id, stream)? {
        return Ok(false);
    }
    let text: Option<String> = tx
        .query_row(
            "SELECT summary FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![grant.station_id.as_str(), grant.session_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let running = text
        .and_then(|t| serde_json::from_str::<SessionSummary>(&t).ok())
        .is_some_and(|s| turn_running(&s));
    if !running {
        return Ok(false);
    }
    let n = tx.execute(
        "UPDATE tokens SET last_used_at = MAX(COALESCE(last_used_at, ?2), ?2)
         WHERE grant_id = ?1 AND state = 'active'",
        params![grant.grant_id, now],
    )?;
    Ok(n == 1)
}

/// The sweep pass: touches every active grant of a fresh station whose
/// session runs a turn. Returns the touched grant ids.
pub(crate) fn touch_running_turns(
    tx: &Connection,
    fresh: &BTreeMap<StationId, Stream>,
    now: i64,
) -> Result<Vec<String>>;
// Body: `super::active_grants(tx)?`, keep the grants whose `station_id` is a
// key of `fresh`, call `touch_if_running` with that stream, collect the ids
// of the grants that it touched.

/// The event pass: touches the active grants of `sessions` (the sessions of
/// one batch whose folded summary runs a turn) on `station`.
pub(crate) fn touch_turns_of(
    tx: &Connection,
    station: &StationId,
    sessions: &BTreeSet<SessionId>,
    stream: &Stream,
    now: i64,
) -> Result<()>;
// Body: for each session, `SELECT {GRANT_COLUMNS} FROM tokens
// WHERE station_id = ?1 AND session_id = ?2 AND state = 'active'`, map with
// `grant_row` and `finish` (as `active_grants` does), and call
// `touch_if_running` for each.

impl DeckStore {
    /// The stream of the current link of `station` (§3.2 items 1-2), or
    /// `None`.
    #[must_use]
    pub(crate) fn fresh_stream(&self, station: &StationId) -> Option<Stream> {
        let since = self.linked_since(station)?;
        let (info, at) = self.station_info(station)?;
        (at >= since).then(|| (info.incarnation, info.head))
    }

    /// [`Self::fresh_stream`] of every station with an answer.
    #[must_use]
    pub(crate) fn fresh_streams(&self) -> BTreeMap<StationId, Stream> {
        self.station_infos()
            .into_keys()
            .filter_map(|id| self.fresh_stream(&id).map(|s| (id, s)))
            .collect()
    }
}

#[cfg(test)]
#[path = "grant_turns_tests.rs"]
mod tests;
```

Rules for the worker: `in_effect`, `active_grants` and the imports are those of `grant_scopes.rs` (`super::` from the child module). If `GRANT_COLUMNS`, `grant_row` or `finish` are not visible from the child module, the worker stops and asks; it does not change their visibility on its own. `fresh_stream` locks `reachable` and `infos` one after the other, never at the same time.

**The sweep** (`grant_scopes.rs:216-237`, `sweep_grants`) changes to:

```rust
pub async fn sweep_grants(&self) -> Result<BTreeSet<StationId>> {
    let fresh = self.fresh_streams();
    let clock = self.inner.clock.clone();
    let (ended, changed, touched) = call(&self.inner.db, move |conn| {
        let tx = conn.transaction()?;
        // The grant stations rule GS2: the time of the decision is read in
        // its own transaction.
        let now = super::now_ms(&clock);
        let touched = turns::touch_running_turns(&tx, &fresh, now)?;
        let ended = end_stale(&tx, now)?;
        super::worker_grant::prune(&tx, now)?;
        let changed = sync_coordinate_scopes(&tx, now)?;
        tx.commit()?;
        Ok((ended, changed, touched.len()))
    })
    .await?;
    if touched > 0 {
        tracing::debug!(touched, "running turns kept their grants in use");
    }
    // The audit rows and the notes stay as they are.
    ...
}
```

The touch comes before `end_stale` in the same transaction. So a grant whose session works at the sweep is in use, and a grant that was already idle past its TTL ends (the touch skips it, §3.1).

**The event fold** (`read_model.rs:175-276`, `apply_events`):

1. Before the database call: `let stream = self.fresh_stream(&station);` and `let clock = self.inner.clock.clone();`.
2. In the loop (`read_model.rs:214-245`), right after `summary.apply(&e)`: keep `running: BTreeSet<SessionId>`; insert `e.session_id` when `turns::turn_running(&summary)` is true, else remove it. The last event of a session in the batch decides.
3. After `set_cursor` (`read_model.rs:249`) and before `tx.commit()`:
   ```rust
   if let Some(stream) = &stream
       && !running.is_empty()
   {
       let now = super::now_ms(&clock);
       super::grant_scopes::turns::touch_turns_of(&tx, &station, &running, stream, now)?;
   }
   ```
   The cursor check of §3.2 item 3 reads the cursor of this batch, because `set_cursor` ran before it in the same transaction.

`apply_snapshot` gets no touch: the next sweep (at most 30 s) covers a snapshot. No other path changes.

**The lazy checks need no change.** They read `last_used_at`, and the touches keep it at most 30 s old while a turn runs on a fresh station. The rule is exact for every `idle_ttl_ms` of 60 s or more. That covers every board path (default 30 min, worker 5-1440 min). An API grant with a shorter idle time can still idle out during a turn that sends no event for longer than that time (§3.9).

### 3.4 Which grants

| Grant | Extension | Reason |
| --- | --- | --- |
| Spawn-time coordinator grant (ADR-006), bound | Yes | The owner decision. |
| Owner-given grant (MCP P4b, kind `coordinator` with a `session_owner_grant` row), bound | Yes | The rule does not read the row; the grant is a coordinator grant bound to a session. |
| Worker grant (MCP P4a, kind `worker`), bound | Yes | A bound worker grant is the grant of a working session in the same way. It stays in the worker cap while it is in use (`WORKER_GRANT_CAP`), which is correct. |
| Any grant without a binding | No | An unbound coordinator grant never idles (§1.1 item 2). An unbound worker grant idles from its creation: that is the cleanup of a lost handoff, and a touch would keep a credential that never reached a provider. |
| Revoked or expired grant | No | No revival (§3.1). |

### 3.5 The texts (`store/grant_end.rs`, new)

A new module `crates/fleetdeck-deck/src/store/grant_end.rs`, declared in `store/mod.rs` as `pub(crate) mod grant_end;` next to `pub mod grant_scopes;` (`store/mod.rs:66`):

```rust
//! Why a grant is not in effect, and the text that a refused call of its
//! session shows (G6, design `grant-idle.md` §3.5).

use std::collections::BTreeMap;

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, params};

use super::grant_scopes::in_effect;
use super::grants::{GRANT_COLUMNS, Grant, GrantState, caller_grant, finish, grant_row};
use super::{DeckStore, Result, call};

/// Why a grant is not in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantEnd {
    /// Idle longer than its idle time: stored `expired`/`idle`, or still
    /// `active` and idle (the sweep has not run yet).
    Idle { idle_ttl_ms: i64 },
    /// Past `expires_at`, or stored `expired` for another reason.
    Lifetime,
    Revoked,
}

/// Why `grant` is not in effect at `now`; `None` while it is in effect.
/// The order: revoked, then the lifetime, then the idle time.
#[must_use]
pub fn grant_end(grant: &Grant, now: i64) -> Option<GrantEnd> {
    let idle = GrantEnd::Idle { idle_ttl_ms: grant.idle_ttl_ms };
    match grant.state {
        GrantState::Revoked => Some(GrantEnd::Revoked),
        _ if now > grant.expires_at => Some(GrantEnd::Lifetime),
        GrantState::Expired if grant.revoked_reason.as_deref() == Some("idle") => Some(idle),
        GrantState::Expired => Some(GrantEnd::Lifetime),
        GrantState::Active if in_effect(grant, now) => None,
        GrantState::Active => Some(idle),
    }
}

/// The text of a refused call (exact strings; tests compare them).
#[must_use]
pub fn end_text(end: GrantEnd) -> String {
    match end {
        GrantEnd::Idle { idle_ttl_ms } => format!(
            "Your fleet grant expired (idle for {}). The owner can renew it on the board.",
            duration_text(idle_ttl_ms)
        ),
        GrantEnd::Lifetime => "Your fleet grant expired (its lifetime ended). \
            The owner can give a new grant on the board."
            .to_owned(),
        GrantEnd::Revoked => {
            "Your fleet grant was revoked. The owner can give a new grant on the board.".to_owned()
        }
    }
}

/// Whole minutes as minutes, else seconds rounded up (at least 1).
fn duration_text(ms: i64) -> String {
    let unit = |n: i64, one: &str| {
        if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
    };
    if ms >= 60_000 && ms % 60_000 == 0 {
        unit(ms / 60_000, "minute")
    } else {
        unit(((ms.max(1) + 999) / 1000).max(1), "second")
    }
}

/// The text for a refused call of `session` on `station`: the end of its
/// newest grant (`caller_grant`), or `None` when it has no grant or the
/// grant is in effect.
pub(crate) fn grant_refusal(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<Option<String>> {
    Ok(caller_grant(conn, station, session)?
        .and_then(|g| grant_end(&g, now))
        .map(end_text))
}

/// The end of the newest grant of each session of `station` whose newest
/// grant is not in effect (the order of `caller_grant`: `created_at`, then
/// `grant_id`).
pub(crate) fn station_grant_ends(
    conn: &Connection,
    station: &StationId,
    now: i64,
) -> Result<BTreeMap<SessionId, GrantEnd>>;
// Body: `SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1
// ORDER BY created_at ASC, grant_id ASC`; insert each grant into a map by
// session (a later row replaces an earlier one); then keep the sessions
// whose grant has `grant_end(&g, now) == Some(_)`.

impl DeckStore {
    /// [`station_grant_ends`] at the time read in the call (§3.6).
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn grant_ends(
        &self,
        station: StationId,
    ) -> Result<BTreeMap<SessionId, GrantEnd>> {
        let clock = self.inner.clock.clone();
        call(&self.inner.db, move |conn| {
            station_grant_ends(conn, &station, super::now_ms(&clock))
        })
        .await
    }
}

#[cfg(test)]
#[path = "grant_end_tests.rs"]
mod tests;
```

Examples: 1 800 000 ms → "idle for 30 minutes"; 60 000 → "1 minute"; 90 000 → "90 seconds"; 1 000 → "1 second"; 1 500 → "2 seconds".

**The store refusals.** Only the message changes; the code stays `forbidden`, the order of the checks stays, and nothing new is stored.

`decide_coordinator` (`coordinator.rs:202-257`), for a call with a grant id (`!worker`):

1. `!granted` (`:215-219`) and `!station_covered` (`:222-223`): first `if !worker && let Some(text) = grant_refusal(&tx, &station, &caller_session, now)? { return Ok(rejected(RejectCode::Forbidden, &text)); }`, then the old text. A worker call keeps the old texts.
2. `grant.state != Active || now > expires_at` (`:234-238`): `end_text(grant_end(&grant, now).unwrap_or(GrantEnd::Lifetime))`.
3. The idle branch (`:246-254`): `expire_idle` as today, then `end_text(GrantEnd::Idle { idle_ttl_ms: grant.idle_ttl_ms })`.
4. The module doc rule 3 and rule 6 (`coordinator.rs:10-25`) add: "A running turn of the grant's session also moves `last_used_at` (G6, `grant_turns.rs`)" and "A refused call of a grant that ended says why (`grant_end.rs`)".

`coordinator_query::check` (`coordinator_query.rs:93-152`), the same rules: items 1 (`"no \`coordinate\` grant"` and `NOT_COVERED`), 2 (`"the grant is revoked or expired"`) and 3 (`"the grant expired after its idle time"`). The "the caller has no grant" text stays.

`authority_check` (`link/requests.rs:167-176`, `:234-235`) keeps `NOT_COVERED`: the asking station is a target, not the caller of a grant. The desktop admission and list keep their fixed P6 codes (§3.9).

### 3.6 The link refusal: `GrantSource::refusal`

**`fleetdeck-link`** (`crates/fleetdeck-link/src/grants.rs`): the trait gets a provided method, and the module gets one helper.

```rust
pub trait GrantSource: fmt::Debug + Send + Sync {
    fn scopes(&self, peer: &Fingerprint) -> BTreeSet<Scope>;

    /// The message of a request from `peer` that the scope check refused,
    /// when this source can say why the scope is missing (G6). `None`
    /// keeps the generic text. It runs in the read loop of the link: no
    /// blocking, no I/O.
    fn refusal(&self, _peer: &Fingerprint, _method: &str, _params: &serde_json::Value) -> Option<String> {
        None
    }
}

/// `e` with the text of `source` when `e` is the scope refusal of
/// `methods::authorize` (code `forbidden`). The code and the decision stay.
pub(crate) fn explain(
    source: &dyn GrantSource,
    peer: &Fingerprint,
    method: &str,
    params: &serde_json::Value,
    mut e: WireError,
) -> WireError {
    if e.code == crate::methods::code::FORBIDDEN
        && let Some(text) = source.refusal(peer, method, params)
    {
        e.message = text;
    }
    e
}
```

`control.rs:729` changes to:

```rust
Err(e) => Err(Outcome::Err(crate::grants::explain(&*p.grants, &p.peer, &method, &params, e))),
```

`control.rs` stays at 785 lines (limit 800). `Grants` and every other implementor keep the default.

**The deck** (`crates/fleetdeck-deck/src/peers.rs`): `DeckPeers` keeps the ends beside the scopes.

1. New field `ends: std::sync::RwLock<HashMap<Fingerprint, BTreeMap<SessionId, GrantEnd>>>`.
2. `load` (`peers.rs:43-61`): for each station, `ends.insert(fp, store.grant_ends(st.station_id.clone()).await?)`.
3. `refresh_one` (`peers.rs:113-121`): read the station and `grant_ends` first, then write `ends`, then `cache.grant(...)`. So a refusal never reads ends that are older than the scopes that caused it.
4. `revoked` (`peers.rs:192-203`): remove the `ends` entry together with `cache.revoke`, before the store write.
5. `rotated` (`peers.rs:205-215`): move the `ends` entry from the old fingerprint to the new one.
6. `impl GrantSource for DeckPeers` gets:
   ```rust
   fn refusal(&self, peer: &Fingerprint, method: &str, params: &Value) -> Option<String> {
       let caller = match method {
           "coordinator.call" => params.get("call")?.get("caller")?.as_str()?,
           "coordinator.query" => params.get("caller")?.as_str()?,
           _ => return None,
       };
       let ends = self.ends.read().unwrap_or_else(PoisonError::into_inner);
       ends.get(peer)?.get(&SessionId::from(caller)).copied().map(end_text)
   }
   ```

Why the ends are fresh enough: a link refusal of `coordinate` means that the station has no grant in effect. That state comes only from a scope drop, and every scope drop refreshes the station (`note_scope_changes` → `refresh_changed`). The refresh reads the ends in the same pass. A renewal makes the grant active again, adds the scope and refreshes, so its end disappears.

### 3.7 What the session sees

| Path | Before | After |
| --- | --- | --- |
| Query, station lost `coordinate` | `` forbidden: `coordinator.query` needs scope coordinate, which the pairing did not grant `` | `forbidden: Your fleet grant expired (idle for 30 minutes). The owner can renew it on the board.` |
| Query, station still covered by another grant | `{"reject":{"code":"forbidden","message":"the grant expired after its idle time"}}` | the same shape with the new message |
| Call, store decides | `forbidden: the coordinator grant expired after its idle time` | `forbidden: Your fleet grant expired (idle for 30 minutes). The owner can renew it on the board.` |
| Call through the outbox, station lost `coordinate` | `{command_id, state: "pending"}`; `command_status` gives the link text | `{command_id, state: "pending"}`; `command_status` gives the new text |
| Lifetime ended | the texts above | `forbidden: Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.` |
| Revoked | the texts above | `forbidden: Your fleet grant was revoked. The owner can give a new grant on the board.` |
| A session without any grant, a worker `ask_coordinator` | unchanged | unchanged |

The MCP server instructions already say "A refused call (forbidden, expired, revoked) is final: do not retry it in a loop; tell the owner what you need." The new texts fit that rule.

### 3.8 Compatibility

- No schema change, no migration, no wire type change, no feature flag. The station does not change.
- An older station shows the new texts as they come (`"{code}: {message}"`, §1.4 item 5).
- `GrantSource::refusal` has a default: `Grants`, `StationPeers` and the test fakes compile unchanged.
- `GrantView.last_used_at` (`crates/fleetdeck-proto/src/orchestration.rs:391`) moves while a turn runs. The board does not show it (`web/src/fleet/FleetAccess.tsx:41` shows only the end reason).
- ADR-006 §5.3 (`ADR-006-coordinator-mcp.md:84`) gets an amendment: a running turn of the grant's session, as the deck sees it on a fresh station, also refreshes `last_used_at` (G6).
- Tests that compare the old texts change: `store/coordinator_tests.rs:274`, `store/grant_scopes_tests.rs:413`, `tests/grant_stations.rs:443-450` (`assert_not_covered`, used by two call tests) and `:493-497` (the query test). The `authority_check` tests (`grant_stations.rs:222`, `:563`) and the worker test (`grant_scopes_tests.rs:366`) keep `NOT_COVERED`.
- Behavior change: a bound grant of a working session lives longer, up to `expires_at`.

### 3.9 Out of scope

- Background work without a run (§8 Q1).
- The desktop tools: their results have a closed P6 vocabulary (`desktop_forward.rs:407`, admission `admit.rs:105-127`). They keep `not_granted` and `desktop_unsupported`. The touch also keeps their grants in use, because they read `last_used_at`.
- The outbox keeps a link-refused call `pending` until `retry_until` (`outbox.rs:280`). A link `forbidden` can be short (the cache follows a renewal a moment later), so this draft does not make it final. `command_status` shows the new text.
- `fleet_view` "me" does not show the end of a grant (`fleet_view_build.rs:276`).
- A grant with `idle_ttl_ms` below 60 s (API only, never from the board) can still idle out during a turn that sends no event for longer than that time.
- The board: no change. Renew is unchanged.

## 4. Errors and edge cases

1. **Deck restart during a long silent turn.** The last touch is at most 30 s before the stop. The start sweep (`store/mod.rs:439`) ends the grant only when the deck was down longer than about `idle_ttl_ms - 30 s`. Then the station links, answers `station.info`, replays, and the next sweep touches again.
2. **Station link loss during a turn.** No touch while the link is down. The grant can idle out; a call during the loss cannot reach the deck anyway. After the link is back, a call that the outbox sends again meets the idle rule as it stands.
3. **Turn starts near the idle limit.** The `run.started` batch touches at once (§3.3), when the station is fresh and the grant is still in effect.
4. **Turn ends.** The batch with `run.completed` does not touch (the folded summary has no run). The clock starts at the previous touch.
5. **Two links of one station (NV-12).** `linked_since` keeps the first `up`; the newest `station.info` is from one of the live links; both are of the current continuous link.
6. **A summary that does not parse** gives no touch (fail closed); the existing fold would fail on it first.
7. **Clock.** Deck clock only; read in the transaction; `MAX` never moves `last_used_at` back.
8. **The moved or resumed session.** The touch uses `grant.station_id` and `grant.session_id`. A session that is now on another station gets no touch.

## 5. Threat notes and risk class

**High.** The change makes grants live longer (authorization), and it changes the text of an authorization refusal. ADR-006 §5.3 changes.

| # | Threat | Defense |
| --- | --- | --- |
| T1 | The coordinator keeps its grant alive without work (a turn that sleeps or loops). | Accepted by the owner decision: a running turn is use. `expires_at` stays the hard cap (default 12 h, at most 7 days). The owner sees the session as Working and can revoke at any time. |
| T2 | A session, a worker or a board user fakes a running turn of the coordinator. | No API writes the read model or `last_used_at`. The run state comes only from the station's event stream of that session. Mail or a question to the coordinator starts real work, which is use. |
| T3 | A paired station claims that its sessions run. | The touch reads only the summary of `grant.station_id`, the station that hosts the session and holds the binding. No station can extend a grant of another station. A compromised station already holds the binding; `expires_at` caps the gain. |
| T4 | A stale read model extends a grant (link down, deck restart, replay not applied). | The freshness rule of §3.2 fails closed; no touch without all three items. |
| T5 | A turn revives an ended grant. | The touch needs `in_effect` at the touch time and `state = 'active'` in the `UPDATE`. Revoked, expired, idle-past-TTL and past-lifetime grants never change. Renew stays the only revival. |
| T6 | A race between the touch and an idle check or a revoke. | One database thread. The touch and `end_stale` share one transaction. A revoke in another transaction sets `state = 'revoked'`, and the `UPDATE` of the touch matches only `active`. A touch only makes `last_used_at` newer. |
| T7 | The refusal text leaks data. | It names only the end reason and the idle time of the newest grant of a session of the calling station, to that station. No grant id, no other station. A session without a grant gets the generic text. |
| T8 | The refusal hook admits a request or blocks the link. | The hook returns only `Option<String>`, and `explain` changes only `message`, only for `forbidden`. It reads an in-memory map under a read lock and fields of the parsed params; no I/O. |
| T9 | The sweep costs more. | One `SELECT` of the active grants (already done), plus one row read and one summary parse for each active bound grant of a fresh station, every 30 s. |
| T10 | A lost handoff keeps a worker credential. | Unbound grants get no touch (§3.4); the idle-from-creation cleanup stays. |

## 6. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. A worker compiles only (`cargo check -p <crate> --tests`) and keeps each file under 800 lines (`wc -l`). The tests run at the merge into `v2` (DL-520).

T1 and T2 own different files and can run in parallel. T3 needs T2 (`GrantEnd`, `grant_ends`) and T1 (the checklist row names its tests).

### T1 (backend): a running turn is use of a bound grant (≈ 120 lines of code, ≈ 300 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/grant_scopes.rs`
  - `crates/fleetdeck-deck/src/store/grant_turns.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_turns_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/read_model.rs`
  - `docs/v2/adr/ADR-006-coordinator-mcp.md` (the §5.3 amendment of §3.8, one sentence at line 84, with "G6, DL-522")
- **Changes:** §3.1 (`turn_running`), §3.2 and §3.3 (the module, the sweep, the event fold). Update the module doc of `grant_scopes.rs` (lines 17-19, the sweep bullet): the sweep also records running turns as use.
- **Tests** (`grant_turns_tests.rs`; own helpers: an in-memory `DeckStore` with `Det`, one or two pinned stations, a snapshot with `SessionSummary::new(..)` plus `current_run`/`state`, `set_reachable`, `set_station_info` with an `info()` like `store/coordinator_tests.rs:54`, a grant from `create_grant`, a binding through a `coordinator.bound` event like `grant_scopes_tests.rs:93-108`):
  1. `a_running_turn_keeps_a_bound_grant_in_effect_past_its_idle_time`: idle 60 s; running; advance 50 s, sweep: `last_used_at == now`; advance 50 s, sweep: still `active`.
  2. `the_idle_clock_starts_at_the_last_moment_the_turn_was_seen`: after test 1, `apply_events` with `run.completed`: `last_used_at` unchanged; advance 59 s, sweep: `active`; advance 2 s, sweep: `expired`/`idle`, one `coordinator.grant_expired` audit row with detail `idle`.
  3. `a_run_started_event_touches_the_grant_in_its_own_batch`: summary without a run; advance 59 s; `apply_events` with `run.started`: `last_used_at == now` without a sweep.
  4. `no_extension_while_the_station_is_unreachable`: running, not reachable; advance 61 s, sweep: `expired`/`idle`.
  5. `no_extension_with_a_station_info_from_an_earlier_link`: info at t0; down; advance 1 s; up: no touch; the grant ends after its idle time.
  6. `no_extension_before_the_read_model_reaches_the_reported_head_or_with_another_incarnation`: info head above the cursor: no touch; info with incarnation `inc-2`: no touch.
  7. `no_extension_for_an_exited_or_lost_session_or_for_background_work_only`: `Exited` with a run, `Lost` with a run, and no run with `background_tasks = ["t1"]`: none touched.
  8. `a_turn_never_revives_a_grant_that_is_idle_past_its_time_past_its_lifetime_or_revoked`: each with a running turn; after the sweep: `expired`/`idle`, `expired`/`expired`, `revoked` with `last_used_at` unchanged.
  9. `an_unbound_grant_gets_no_extension`: an unbound coordinator grant keeps `last_used_at == None`; an unbound worker grant (made like `worker_grant_tests.rs` makes one) ends `idle` from its creation.
  10. `a_bound_worker_grant_is_extended`.
  11. `the_turn_of_another_session_or_another_station_does_not_extend_a_grant`.
  12. `a_query_after_a_long_silent_turn_is_admitted`: idle 60 s; running; advance 50 s, sweep, advance 50 s, sweep; `coordinator_query` `ListSessions` with the binding: accepted.
- **depends_on:** none.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_turns) | test(grant_scopes) | test(coordinator) | test(worker_grant) | test(owner_grants) | test(read_model) | binary(grant_stations) | binary(worker_grant) | binary(live_grant_routes)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`

### T2 (backend): the store refusals say why a grant ended (≈ 110 lines of code, ≈ 150 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/mod.rs` (one `mod` line)
  - `crates/fleetdeck-deck/src/store/grant_end.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_end_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/coordinator.rs`
  - `crates/fleetdeck-deck/src/store/coordinator_tests.rs`
  - `crates/fleetdeck-deck/src/store/coordinator_query.rs`
  - `crates/fleetdeck-deck/src/store/grant_scopes_tests.rs`
  - `crates/fleetdeck-deck/tests/grant_stations.rs`
- **Changes:** §3.5 (module, the `decide_coordinator` and `check` texts, the module doc of `coordinator.rs`). Change the old text assertions of §3.8.
- **Tests:**
  - `grant_end_tests.rs`:
    1. `grant_end_orders_revoked_then_lifetime_then_idle`: revoked → `Revoked`; `expired`/`idle` in its lifetime → `Idle`; `expired`/`idle` past its lifetime → `Lifetime`; `expired`/`expired` → `Lifetime`; `active` past `expires_at` → `Lifetime`; `active` idle past its TTL → `Idle`; `active` in effect → `None`; unbound coordinator grant → `None`.
    2. `end_text_says_why_and_what_the_owner_can_do`: the three exact texts of §3.5 and the five duration examples.
    3. `station_grant_ends_keeps_the_newest_grant_of_each_session_of_the_station`: `coord-1` with an older revoked and a newer idle-expired grant → `Idle`; `coord-2` with a grant in effect → absent; a grant of another station → absent.
  - `coordinator_tests.rs`: line 274 expects `"Your fleet grant expired (idle for 1 second). The owner can renew it on the board."`; new test `a_refused_call_of_an_ended_grant_says_why`: lifetime ended (another grant keeps the station covered) → the lifetime text; revoked → the revoked text; the only grant idle and the station no longer covered → the idle text, not `NOT_COVERED`.
  - `grant_scopes_tests.rs`: line 413 expects the idle text with "1 second"; new test `a_refused_query_of_an_ended_grant_says_why` (lifetime and revoked through `coordinator_query`).
  - `tests/grant_stations.rs`: `assert_not_covered` becomes `assert_refused(receipt, text)`; the lapse test expects the lifetime text, the idle test the idle text with "1 second"; the query test (`:493-497`) the lifetime text. `:222` and `:563` stay.
- **depends_on:** none.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_end) | test(coordinator) | test(grant_scopes) | binary(grant_stations) | binary(coordinator_fleet) | binary(worker_grant)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1).
  - `grep -rn 'expired after its idle time\|grant is revoked or expired' crates/` prints nothing.

### T3 (backend): the link refusal says why (≈ 80 lines of code, ≈ 220 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-link/src/grants.rs`
  - `crates/fleetdeck-link/src/control.rs` (line 729 only)
  - `crates/fleetdeck-link/src/control_tests.rs` (one `#[path]` module pair at the end, like `control_rx_tests.rs` at lines 697-701)
  - `crates/fleetdeck-link/src/control_refusal_tests.rs` (new)
  - `crates/fleetdeck-deck/src/peers.rs`
  - `crates/fleetdeck-deck/src/peers_tests.rs` (new, declared at the end of `peers.rs`)
  - `crates/fleetdeck-deck/tests/grant_refusal_text.rs` (new, with `mod common;`)
  - `docs/v2/SECURITY-CHECKLIST.md` (one row `522` after the last row: G6, the files of T1-T3, threats T1-T10 of §5 in short, the test names of T1 and T3)
- **Changes:** §3.6.
- **Tests:**
  - `control_refusal_tests.rs` (a fake `GrantSource` with no scopes, a call counter, and `refusal` that returns `Some(format!("why {method} {caller}"))` with `caller = params["caller"]`; build the channel like `side()` in `control_tests.rs:81-86` with this source):
    1. `a_scope_refusal_takes_the_text_of_the_grant_source`: `coordinator.query` with `{"caller": "c1"}` → `Outcome::Err` code `forbidden`, message `why coordinator.query c1`.
    2. `a_source_without_a_text_keeps_the_generic_refusal`: `refusal` returns `None` → `` `coordinator.query` needs scope coordinate, which the pairing did not grant ``.
    3. `the_source_is_asked_only_for_a_scope_refusal`: a wrong-role method keeps its `wrong_role` text and the counter stays 0.
  - `peers_tests.rs`:
    1. `load_fills_the_end_texts_from_the_store`: an idle-expired bound grant of `coord-1`; `DeckPeers::load`; `refusal(fp, "coordinator.query", {"caller":"coord-1"})` is the idle text.
    2. `refusal_answers_only_for_the_calling_station_its_sessions_and_the_two_methods`: another fingerprint → `None`; `coordinator.authority_check` → `None`; params without a caller → `None`; `coordinator.call` reads `call.caller`.
    3. `a_revoked_key_has_no_end_texts_and_a_rotated_key_keeps_them`.
  - `tests/grant_refusal_text.rs` (the fixture of `common`, `f.connect()`, `common::fleet::bound_edit` and `common::fleet::query`):
    1. `a_link_refused_query_of_an_idle_expired_grant_says_why`: bound grant of the fixture station, idle 1 s; advance 2 s; `sweep_grants`; `peers.refresh_changed()`; `coordinator.query` over `station.control` → `forbidden` with the idle text ("1 second").
    2. `a_link_refused_call_of_a_lifetime_ended_or_revoked_grant_says_why`: `coordinator.call` with a command id → the lifetime text; after `revoke_grant` and a refresh → the revoked text.
    3. `a_caller_without_a_grant_keeps_the_generic_text`.
- **depends_on:** T2 (and T1 for the checklist row).
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(control) | test(methods)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(peers) | binary(grant_refusal_text) | binary(grant_stations) | binary(deck_link)'`
  - `cargo clippy -p fleetdeck-link -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1). `wc -l crates/fleetdeck-link/src/control.rs` is at most 800.

### At the merge (coordinator)

- `make check` once, after T3.
- The adversary PRE-PR REVIEW of the final branch (high risk), with the acceptance commands above as its fast checks.

## 7. Decisions made in this draft

1. **A running turn is `current_run` set and the process alive** (§3.1). Rejected: the card status Working (it includes background work, which the owner decision does not name); `current_turn` (a run can sit between turns).
2. **The extension is recorded as use (`last_used_at`), not computed at each check.** It survives a deck restart, and no idle check changes. Rejected: a predicate inside `in_effect` (it is pure and used in about ten places; it cannot read the read model); a new column `turn_seen_at` (a migration for no gain).
3. **Two touch points: the sweep (30 s) and each event batch.** The batch closes the gap at the start of a turn. Rejected: the sweep only (a turn that starts in the last 30 s of the idle time loses its grant); a touch at the end of a turn (a replay after a disconnect would date it to the replay).
4. **Freshness is "link up + `station.info` of this link + cursor at its head"** (§3.2). Rejected: reachability only (the read model of the old link can say "running" until the replay lands); `SnapshotMark` (a plain replay never sets it).
5. **No revival:** the touch needs the grant in effect at the touch time.
6. **Every bound grant, of both kinds, gets the extension; unbound grants do not** (§3.4).
7. **The clock is read in the transaction of the touch,** also in `sweep_grants` (the GS2 rule of `grant_scopes.rs:11-16`).
8. **The text comes from the deck at both refusal points:** the store checks, and a new provided method `GrantSource::refusal` at the link scope check. Rejected: a station-side rewrite (the station does not know the reason); keeping `coordinate` for a station whose grant ended (weaker authorization); a new reject code (an older station cannot decode an unknown `RejectCode`).
9. **The code stays `forbidden`;** only the message changes.
10. **Three texts** (idle, lifetime, revoked); the idle text names the idle time of the grant, in whole minutes or in seconds.
11. **The ends live in memory beside the link scopes** and are refreshed with them. Rejected: a database read in the link read loop (it must not block).
12. **The desktop tools, the outbox and `fleet_view` stay unchanged** (§3.9).
13. **Risk class high:** the adversary does the DESIGN REVIEW and the PRE-PR REVIEW.

## 8. Owner questions

1. **Does background work count?** The backlog case is "its own long workflow only waits". A Claude Code workflow or a background subagent runs after its turn ended; the board shows that session as Working, but no turn runs. Under the decision of 2026-10-24 the grant can still idle out then. Option: count agent work, `current_run.is_some() || running_agents > 0` (a running workflow or subagent), but not a background shell task (a dev server would keep the grant alive). **Default: no (the decision as written).** The change is one line in `turn_running` and one test.
2. **Do bound worker grants (MCP P4a) get the extension?** **Default: yes** (§3.4).

## Decision log entries (to append)

## DL-522: G6 a running turn of the session of a bound grant counts as use (owner decision 2026-10-24; design agent, 2026-10-24)

- Context: B24 G6; design `docs/v2/design/grant-idle.md` §3.1-§3.4. A coordinator grant went idle after 30 min while its session worked without fleet calls.
- Decision: the deck moves `last_used_at` of a bound grant in effect to the deck time while its session has a running turn (`current_run` set, process not exited or lost) in a fresh read model. The sweep does it every 30 s, before `end_stale` in the same transaction, and each event batch does it for the sessions whose folded summary runs a turn. The idle clock starts at the last moment the deck saw the turn. `expires_at` stays the hard cap. A touch never revives a grant that is not in effect. The rule applies to every bound grant: spawn-time, owner-given (P4b) and worker (P4a). No schema change. ADR-006 §5.3 is amended.
- Rejected: an idle predicate that reads the read model at each check; a new column; a touch at the end of a turn.

## DL-523: G6 a station read model is fresh when its link is up, `station.info` came on that link and the cursor reached its head (design agent, 2026-10-24)

- Decision: the touch of DL-522 needs `linked_since(station)`, a `station.info` answer at or after it, and in the transaction `stations.incarnation_id == info.incarnation` and `stations.cursor >= info.head`. Else no touch (fail closed): link down, deck restart, replay or snapshot not applied, an answer from an earlier link.
- Rejected: reachability only; `SnapshotMark` (a replay without a snapshot never sets it).

## DL-524: G6 a refused call of a grant that ended says why (design agent, 2026-10-24)

- Decision: `store/grant_end.rs` gives `GrantEnd` (idle, lifetime, revoked) and three texts: "Your fleet grant expired (idle for N minutes). The owner can renew it on the board.", "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.", "Your fleet grant was revoked. The owner can give a new grant on the board." The store checks of `decide_coordinator` and `coordinator.query` use them for a caller with a grant. The link scope check asks the new provided method `GrantSource::refusal`; `DeckPeers` answers from an in-memory map of the ends that it refreshes with the scopes. The code stays `forbidden`. A worker call, `authority_check` and the desktop tools keep their texts.
- Rejected: a station-side rewrite; keeping `coordinate` for a station whose grant ended; a new reject code.

## DL-525: G6 is high risk: adversary design review and pre-PR review (design agent, 2026-10-24)

- Decision: G6 changes how long a grant authorizes calls and the text of an authorization refusal. It gets the adversary DESIGN REVIEW before T1-T3 start and the PRE-PR REVIEW of the final branch. `SECURITY-CHECKLIST.md` gets row 522.
