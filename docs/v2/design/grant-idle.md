# A running turn keeps a grant in use, and a refused call says why (G6)

- **Status:** Draft for the architect, revision 2 (2026-10-24, after the adversary DESIGN REVIEW round 1; see "Revision 2" at the end). Risk: **high** (an authorization change, §5). The adversary does a DESIGN REVIEW before the design is frozen and a PRE-PR REVIEW before the merge.
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
   - The time of an entry is not the time of the answer and does not identify the link that answered. `record_station_info` first awaits `remember_machine` (a database write) and only then calls `set_station_info`, which stamps the deck time of that moment (`crates/fleetdeck-deck/src/store/station_machines.rs:19-22`; `store/mod.rs:622`). `info_loop` does not check that its link still lives before it publishes (`link/info.rs:63-95`). So an answer of a link that ended can be published after a new link came up, with a time after the new `linked_since` (revision 2, finding R2-1).
6. Each link has a unique token: `DeckLink::up` takes `next_token.fetch_add(1)` (`link/mod.rs:121-146`, token at `:128`) and records it with `set_link_features(id, token, ..)` before `fetch_info` starts (`link/mod.rs:207`, `:216`). `LiveGuard::drop` calls `clear_link_features(id, token)` when the link ends (`link/mod.rs:171-182`, `:176`). So the keys of `link_features` (`store/mod.rs:275`; `set_link_features` `:493-498`, `clear_link_features` `:528-543`) are exactly the live links of a station. A token is never used again.
7. Every idle admission check (`decide_coordinator`, `coordinator_query::check`, the desktop admission) and every grant write runs in a closure on the one database thread (`call`, `store/mod.rs:746-751`).
8. The read model stores its station incarnation and cursor in `stations.incarnation_id` and `stations.cursor` (`schema.rs:42-43`; `StationRecord`, `crates/fleetdeck-deck/src/store/stations.rs:60-64`).

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

Revision 2 (R2-1) replaces the time comparison of revision 1 (`info_at >= linked_since`) by the link token. A time does not identify the link that answered (§1.3 item 5).

**The link stream.** The deck keeps, in memory, the `station.info` stream of each live link (the type is defined in `grant_turns.rs`, §3.3):

```rust
/// The incarnation and the head of one `station.info` answer (§3.2).
pub(crate) type Stream = (IncarnationId, u64);
```

- New field of `store::Inner` (`store/mod.rs`, after `infos`): `link_streams: Mutex<BTreeMap<StationId, BTreeMap<u64, Stream>>>` (station → link token → stream), initialized with `Mutex::default()`.
- `fetch_info` gets the token of its link (`link/mod.rs:216` passes `live.token`; the binding `_live` at `:207` is renamed `live`). In its `ask` closure, right after `ask_info` returns `Ok(Some(info))` and before `info_loop` publishes the answer for the board, it calls `ctx.store.record_link_stream(id.clone(), token, (info.incarnation.clone(), info.head)).await` and logs an error with `tracing::warn!`. `info_loop` and `record_station_info` do not change.
- `Inner::set_link_stream(station, token, stream) -> bool` locks `link_features`, and while it holds that lock it checks that `token` is a key of `link_features[station]` (the link lives, §1.3 item 6). Only then it locks `link_streams` and inserts. It returns whether it inserted. An answer of a link that ended is never recorded.
- `Inner::forget_link_stream(station, token)` removes the entry (and the station entry when it is empty). `clear_link_features` calls it as its last statement, after the token left `link_features`. With the lock order of `set_link_stream`, no answer of the ended link can be inserted after the removal.
- `Inner::current_stream(station) -> Option<Stream>` locks `link_features`, then `link_streams`, and returns the stream of the **newest live link that answered**: the highest token that is a key of both maps (newest = highest token, as `link_peer` does at `store/mod.rs:508-515`). `None` when no live link of the station has answered yet.
- Lock order: `link_features`, then `link_streams`. No path takes them in the other order. No path holds either lock across an `await` or a database call.

**Fresh.** A station is fresh in a transaction when both items are true:

1. `inner.current_stream(station) == Some((incarnation, head))`: a live link of the station answered `station.info` on that link (the newest such link decides);
2. the read model has caught up with that answer: `stations.incarnation_id == incarnation` and `stations.cursor >= head`.

The touch reads **both items inside its own database closure**, item 1 from memory and item 2 from the transaction. Nothing is captured before the database call, so there is no stale value to check again. When the deck cannot show both items, there is no touch. Cases:

| Case | Result |
| --- | --- |
| Link down, deck restart before the station links again | No touch (item 1, no live token). The start sweep ends a grant that is idle past its TTL. |
| Link up, `station.info` not answered yet on any live link | No touch (item 1). |
| The answer of a link that ended arrives late, also after a new link came up | Not recorded (`set_link_stream` sees no live token). No touch (item 1). |
| Replay or snapshot not applied up to the reported head | No touch (item 2). |
| The station restarted with a new incarnation and the snapshot is not applied | No touch (item 2, incarnation). |
| A summary that does not parse | No touch. |

A fresh station that is stuck (link alive, no events) keeps its last state. This is the trust that the board already gives the station link; `expires_at` caps it (§5 T3).

### 3.3 Deck store: the touch (`store/grant_turns.rs`, new)

**Touch points.** The deck touches at every step that can make a running session fresh, and in that same database step. Revision 2 (R2-2) adds points 3 and 4 and makes point 2 cover the whole station:

1. **The sweep** (every 30 s): every active bound grant of every station.
2. **Each event batch** (`apply_events`): every active bound grant of the station of the batch, also of a session without an event in the batch. The batch can move the cursor up to the head (§3.2 item 2).
3. **Each snapshot** (`apply_snapshot_with`): every active bound grant of the station. A snapshot sets the incarnation and the cursor (§3.2 item 2).
4. **Each `station.info` answer of a live link** (`record_link_stream`): the publication of the stream (§3.2 item 1) and the touch run in **one closure on the database thread**. No idle admission check can run between them (§1.3 item 7).

So when a station becomes fresh with a running session, `last_used_at` moves before any idle check can read it (one residual with two live links: §4 item 7). While it stays fresh and running, the sweep moves it at least every 30 s, and each batch moves it too.

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
//! and the station is fresh (§3.2). The sweep, each event batch, each
//! snapshot and each `station.info` answer of a live link do it, in the
//! database step that can make the station fresh (§3.3). Every idle check
//! reads `last_used_at`, so it needs no change.

use std::collections::BTreeMap;
use std::sync::Arc;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::SessionSummary;
use fleetdeck_proto::{IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{active_grants, in_effect};
use crate::store::grants::{GRANT_COLUMNS, Grant, finish, grant_row};
use crate::store::{DeckStore, Inner, Result, call};

/// The incarnation and the head of one `station.info` answer (§3.2).
pub(crate) type Stream = (IncarnationId, u64);

/// `turn_running` exactly as in §3.1.

/// True when the read model of `station` has the incarnation of `stream`
/// and its cursor is at least the head (§3.2 item 2).
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

/// Touch point 1 (the sweep): every active grant whose station is fresh.
/// The stream of each station is read once, inside this transaction.
/// Returns the number of touched grants.
pub(crate) fn touch_running_turns(tx: &Connection, inner: &Inner, now: i64) -> Result<usize>;
// Body: `active_grants(tx)?`; keep a `BTreeMap<StationId, Option<Stream>>`
// memo filled with `inner.current_stream(&g.station_id)`; for a grant whose
// station has `Some(stream)`, call `touch_if_running`; count the `true`s.

/// Touch points 2-4: every active grant of `station` (all its sessions).
/// Returns the number of touched grants.
pub(crate) fn touch_station_turns(
    tx: &Connection,
    station: &StationId,
    stream: &Stream,
    now: i64,
) -> Result<usize>;
// Body: `SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1 AND
// state = 'active'` (index `tokens_by_caller`, `schema.rs:264`), map with
// `grant_row` and `finish` (as `active_grants` does), call
// `touch_if_running` for each, count the `true`s.

/// Touch points 2 and 3 in the caller's transaction: reads the stream of
/// `station` (§3.2 item 1) and the clock now, and touches when the station
/// has a stream. Called after `set_cursor`, before `tx.commit()`.
pub(crate) fn touch_after_fold(tx: &Connection, inner: &Inner, station: &StationId) -> Result<usize> {
    match inner.current_stream(station) {
        Some(stream) => touch_station_turns(tx, station, &stream, crate::store::now_ms(&inner.clock)),
        None => Ok(0),
    }
}

impl DeckStore {
    /// Touch point 4: records the stream of a `station.info` answer of link
    /// `token` (§3.2) and touches the running turns of `station`, in one
    /// closure on the database thread. Returns `Ok(0)` without a write
    /// when the link ended (the answer is not recorded).
    ///
    /// # Errors
    /// A database error (the stream stays recorded; the next batch or
    /// sweep touches).
    pub async fn record_link_stream(
        &self,
        station: StationId,
        token: u64,
        stream: Stream,
    ) -> Result<usize> {
        let inner = Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            if !inner.set_link_stream(&station, token, stream.clone()) {
                return Ok(0);
            }
            let tx = conn.transaction()?;
            let now = crate::store::now_ms(&inner.clock);
            let n = touch_station_turns(&tx, &station, &stream, now)?;
            tx.commit()?;
            Ok(n)
        })
        .await
    }
}

#[cfg(test)]
#[path = "grant_turns_tests.rs"]
mod tests;
```

The memory part of §3.2 (`set_link_stream`, `forget_link_stream`, `current_stream`) goes into `store/station_link.rs` as `impl Inner { .. }` (the link presence module; `grant_scopes.rs:149` shows the `impl Inner` pattern of a child module). The exact bodies:

```rust
impl Inner {
    /// Records the stream of link `token` of `station` while that link
    /// lives (G6 §3.2). Lock order: `link_features`, then `link_streams`.
    pub(super) fn set_link_stream(&self, station: &StationId, token: u64, stream: Stream) -> bool {
        let features = self.link_features.lock().unwrap_or_else(PoisonError::into_inner);
        if !features.get(station).is_some_and(|links| links.contains_key(&token)) {
            return false;
        }
        let mut streams = self.link_streams.lock().unwrap_or_else(PoisonError::into_inner);
        streams.entry(station.clone()).or_default().insert(token, stream);
        true
    }

    /// Forgets the stream of link `token` (the link ended).
    pub(super) fn forget_link_stream(&self, station: &StationId, token: u64) {
        let mut streams = self.link_streams.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(links) = streams.get_mut(station) {
            links.remove(&token);
            if links.is_empty() {
                streams.remove(station);
            }
        }
    }

    /// The stream of the newest live link of `station` that answered
    /// `station.info` (G6 §3.2 item 1).
    pub(crate) fn current_stream(&self, station: &StationId) -> Option<Stream> {
        let features = self.link_features.lock().unwrap_or_else(PoisonError::into_inner);
        let live = features.get(station)?;
        let streams = self.link_streams.lock().unwrap_or_else(PoisonError::into_inner);
        streams
            .get(station)?
            .iter()
            .rev()
            .find(|(token, _)| live.contains_key(token))
            .map(|(_, stream)| stream.clone())
    }
}
```

`station_link.rs` imports `std::sync::PoisonError`, `super::Inner` and `super::grant_scopes::turns::Stream`. The field type in `store/mod.rs` is `Mutex<BTreeMap<StationId, BTreeMap<u64, grant_scopes::turns::Stream>>>`. `clear_link_features` (`store/mod.rs:528-543`) gets one last line: `self.inner.forget_link_stream(station, token);`. `store/mod.rs` grows by about 5 lines (from 772); it stays under 800.

Rules for the worker: `in_effect`, `active_grants` and the imports are those of `grant_scopes.rs` (`super::` from the child module). If `GRANT_COLUMNS`, `grant_row`, `finish`, `Inner` or `now_ms` are not visible from the child module, the worker stops and asks; it does not change their visibility on its own. The database closures capture `Arc<Inner>` (`Arc::clone(&self.inner)`); they lock only `link_features` and `link_streams`, briefly, and in that order.

**The sweep** (`grant_scopes.rs:216-237`, `sweep_grants`) changes to:

```rust
pub async fn sweep_grants(&self) -> Result<BTreeSet<StationId>> {
    let inner = Arc::clone(&self.inner);
    let (ended, changed, touched) = call(&self.inner.db, move |conn| {
        let tx = conn.transaction()?;
        // The grant stations rule GS2: the time of the decision is read in
        // its own transaction. The streams are read here too (§3.2).
        let now = super::now_ms(&inner.clock);
        let touched = turns::touch_running_turns(&tx, &inner, now)?;
        let ended = end_stale(&tx, now)?;
        super::worker_grant::prune(&tx, now)?;
        let changed = sync_coordinate_scopes(&tx, now)?;
        tx.commit()?;
        Ok((ended, changed, touched))
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

**The event fold** (`read_model.rs:175-276`, `apply_events`): before the database call, `let inner = Arc::clone(&self.inner);`. After `set_cursor` (`read_model.rs:249`) and before `tx.commit()`: `super::grant_scopes::turns::touch_after_fold(&tx, &inner, &station)?;`. The cursor check of §3.2 item 2 reads the cursor of this batch, because `set_cursor` ran before it in the same transaction. The fold keeps no set of running sessions: the touch reads every grant of the station.

**The snapshot** (`read_model.rs:309-380`, `apply_snapshot_with`): the same two lines, after `set_cursor` (`read_model.rs:375`) and before `tx.commit()`. A snapshot of a superseded link returns `Superseded` before any write (NV-12), so it never touches.

**The lazy checks need no change.** They read `last_used_at`. A station becomes fresh at touch points 2-4, and each of them touches in the same database step (the one residual with two live links is in §4 item 7). While the station stays fresh and the turn runs, the sweep keeps `last_used_at` at most 30 s old. So the rule is exact for every `idle_ttl_ms` of 60 s or more. That covers every board path (default 30 min, worker 5-1440 min). An API grant with a shorter idle time can still idle out during a turn that sends no event for longer than that time (§3.9).

### 3.4 Which grants

| Grant | Extension | Reason |
| --- | --- | --- |
| Spawn-time coordinator grant (ADR-006), bound | Yes | The owner decision. |
| Owner-given grant (MCP P4b, kind `coordinator` with a `session_owner_grant` row), bound | Yes | The rule does not read the row; the grant is a coordinator grant bound to a session. |
| Worker grant (MCP P4a, kind `worker`), bound | Yes | A bound worker grant is the grant of a working session in the same way. It stays in the worker cap while it is in use (`WORKER_GRANT_CAP`), which is correct. |
| Any grant without a binding | No | An unbound coordinator grant never idles (§1.1 item 2). An unbound worker grant idles from its creation: that is the cleanup of a lost handoff, and a touch would keep a credential that never reached a provider. |
| Revoked or expired grant | No | No revival (§3.1). |

### 3.5 The texts (`store/grant_end.rs`, new)

A new module `crates/fleetdeck-deck/src/store/grant_end.rs`, declared in `store/grants.rs` as a child module next to the other `#[path]` children (`grants.rs:27-34`), so that T2 does not share `store/mod.rs` with T1 (revision 2):

```rust
#[path = "grant_end.rs"]
pub(crate) mod grant_end;
```

The path is `crate::store::grants::grant_end`. Its content:

```rust
//! Why a grant is not in effect, and the text that a refused call of its
//! session shows (G6, design `grant-idle.md` §3.5).

use std::collections::BTreeMap;

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, params};

use super::{GRANT_COLUMNS, Grant, GrantState, caller_grant, finish, grant_row};
use crate::store::grant_scopes::in_effect;
use crate::store::{DeckStore, Result, call};

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

/// What the link cache keeps of the ended newest grant of a session
/// (§3.6, revision 2 R2-3): the end when the cache was filled and the
/// lifetime. The end is classified again at each refusal with [`Self::at`],
/// so a lifetime that passes later needs no refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndFacts {
    pub end: GrantEnd,
    pub expires_at: i64,
}

impl EndFacts {
    /// The end at `now`, in the order of [`grant_end`]: revoked stays
    /// revoked; past `expires_at` is the lifetime; else the cached end.
    #[must_use]
    pub fn at(self, now: i64) -> GrantEnd {
        match self.end {
            GrantEnd::Revoked => GrantEnd::Revoked,
            _ if now > self.expires_at => GrantEnd::Lifetime,
            end => end,
        }
    }
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

/// The end facts of the newest grant of each session of `station` whose
/// newest grant is not in effect (the order of `caller_grant`:
/// `created_at`, then `grant_id`).
pub(crate) fn station_grant_ends(
    conn: &Connection,
    station: &StationId,
    now: i64,
) -> Result<BTreeMap<SessionId, EndFacts>>;
// Body: `SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1
// ORDER BY created_at ASC, grant_id ASC`; insert each grant into a map by
// session (a later row replaces an earlier one); then keep the sessions
// whose grant has `grant_end(&g, now) == Some(end)`, as
// `EndFacts { end, expires_at: g.expires_at }`.

impl DeckStore {
    /// [`station_grant_ends`] at the time read in the call (§3.6).
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn grant_ends(
        &self,
        station: StationId,
    ) -> Result<BTreeMap<SessionId, EndFacts>> {
        let clock = self.inner.clock.clone();
        call(&self.inner.db, move |conn| {
            station_grant_ends(conn, &station, crate::store::now_ms(&clock))
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

1. New field `ends: std::sync::RwLock<HashMap<Fingerprint, BTreeMap<SessionId, EndFacts>>>`.
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
       let facts = {
           let ends = self.ends.read().unwrap_or_else(PoisonError::into_inner);
           ends.get(peer)?.get(&SessionId::from(caller)).copied()?
       };
       Some(end_text(facts.at(self.store.now_ms())))
   }
   ```
   `DeckStore::now_ms` reads the deck clock only (no I/O, `store/mod.rs:591`).

**Why the ends stay current (revision 2, R2-3).** A link refusal of `coordinate` means that no grant in effect names the station. The cached facts of a session can go out of date in three ways, and each has its own rule:

1. **A grant ends while it was in effect** (sweep, lazy expiry, a lifecycle event, a snapshot). When it was the last grant in effect of the station, the station loses `coordinate`: a scope drop, which refreshes the station (`note_scope_changes` → `refresh_changed`), and the refresh reads the ends in the same pass. While another grant covers the station, the link does not refuse, and the store texts (§3.5) are computed at the call.
2. **Time passes** (an idle end reaches `expires_at`). `EndFacts::at` classifies at each refusal with the deck time. No refresh is needed. `end_stale` does not visit an ended grant (`active_grants` only), and none is needed.
3. **The state or the reason of an ended grant changes without a scope change** (a revoke of an idle-expired grant; a `coordinator.ended` of the station for a grant that already ended; a snapshot projection that revokes a grant that already ended). Each write of this kind adds the grant's own station (`grant.station_id`, the key of the ends) to the returned set of changed stations, so the caller notes it and `refresh_changed` reads the ends again. The writes, all in T3:
   - `worker_grant::revoke_tx` (`worker_grant.rs:695-712`; every revoke path uses it: the board, a session end, a handoff skip, the owner grants, the worker cap, `owner_ended`): replace the last line `Ok((n == 1, sync_coordinate_scopes(tx, now)?))` by
     ```rust
     let mut changed = sync_coordinate_scopes(tx, now)?;
     changed.extend(super::grants_lifecycle::ended_station(tx, grant_id, n)?);
     Ok((n == 1, changed))
     ```
     `worker_grant.rs` has 796 lines; it gets 798 (limit: below 800). No `use` line is added.
   - `grants_lifecycle::ended_station` (new, `pub(super)`): `None` when `n != 1`, else `SELECT station_id FROM tokens WHERE grant_id = ?1` as `Some(StationId)`.
   - `grants_lifecycle::apply_ended` (`grants_lifecycle.rs:255-278`): keep the count of its `UPDATE` (`let n = tx.execute(..)?`) and return `BTreeSet::from([station.clone()])` when `n > 0`, else the empty set.
   - `grants_lifecycle::owner_ended`, the `expired` branch (`grants_lifecycle.rs:356-366`): when `n == 1`, add `station.clone()` to the returned set.
   - `grants_lifecycle::apply_projection`, the `snapshot_revoked` `UPDATE` (`grants_lifecycle.rs:426-433`): keep its count and add `station.clone()` to `changed` when it is above 0.

   A note of a station whose scopes did not change is harmless: `refresh_one` reads the stored scopes again and publishes the same set. The stale text can only be a wrong reason, never an admission (§5 T12).

A renewal makes the grant active again, adds the scope and refreshes, so its end disappears. The retention prune of worker grants (`worker_grant.rs:770-793`) deletes only grants that ended more than 7 days ago; their cached facts classify as `Lifetime`, which stays true.

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
- Behavior change (revision 2): `revoke_tx`, `apply_ended`, the `expired` branch of `owner_ended` and the `snapshot_revoked` write of `apply_projection` also return the grant's own station when they change a grant row (§3.6). So `refresh_changed` runs for that station more often; it publishes the same scopes. A test that compares the exact returned set of these functions changes with it (the worker lists each one in its report).

### 3.9 Out of scope

- Background work without a run (§8 Q1).
- The desktop tools: their results have a closed P6 vocabulary (`desktop_forward.rs:407`, admission `admit.rs:105-127`). They keep `not_granted` and `desktop_unsupported`. The touch also keeps their grants in use, because they read `last_used_at`.
- The outbox keeps a link-refused call `pending` until `retry_until` (`outbox.rs:280`). A link `forbidden` can be short (the cache follows a renewal a moment later), so this draft does not make it final. `command_status` shows the new text.
- `fleet_view` "me" does not show the end of a grant (`fleet_view_build.rs:276`).
- A grant with `idle_ttl_ms` below 60 s (API only, never from the board) can still idle out during a turn that sends no event for longer than that time.
- The board: no change. Renew is unchanged.

## 4. Errors and edge cases

1. **Deck restart during a long silent turn.** The last touch is at most 30 s before the stop. The start sweep (`store/mod.rs:439`) ends the grant only when the deck was down longer than about `idle_ttl_ms - 30 s`. The start sweep finds no live link, so it touches nothing. Then the station links, answers `station.info`, replays, and the first step that makes the station fresh touches (§3.3 touch points 2-4).
2. **Station link loss during a turn.** No touch while the link is down. The grant can idle out; a call during the loss cannot reach the deck anyway. After the link is back, the step that makes the station fresh touches at once, also when it is the `station.info` answer, a snapshot or a replay batch without an event of the session (R2-2). A grant that idled out during the loss stays ended (no revival); a call that the outbox sends again gets the idle text.
3. **Turn starts near the idle limit.** The `run.started` batch touches at once (§3.3), when the station is fresh and the grant is still in effect.
4. **Reconnect just before the idle deadline.** A grant last used 29 min 59 s ago (idle time 30 min), the link comes back, and the session still runs. The answer of the new link, the snapshot or the replay batch that makes the station fresh touches in its own database step. A query 2 s later is admitted; no sweep is needed (R2-2).
5. **A late answer of an ended link.** The answer was sent on link A; A ended and link B came up before the deck published the answer. `set_link_stream` finds no live token A and does not record it. Only an answer of B can make the station fresh, and only after the read model reached the head of that answer (R2-1).
6. **Turn ends.** The batch with `run.completed` does not touch (the folded summary has no run). The clock starts at the previous touch.
7. **Two links of one station (NV-12).** The stream of the newest live link that answered decides. Every candidate is the answer of a link that still lives. Residual: when the newest answered link ends while an older live link stays, the stream of the older link can make the station fresh without a database step. The next batch or sweep touches within 30 s. A grant loses access in that window only when its session sent no event for nearly its idle time and the window ends after the idle deadline. This needs two live links and the end of the newer one; the design accepts it (fail safe: less extension, never more).
8. **A database error in `record_link_stream`.** The stream stays recorded, the touch of that step is lost, and the next batch or sweep touches. The error is logged.
9. **A summary that does not parse** gives no touch (fail closed); the existing fold would fail on it first.
10. **Clock.** Deck clock only; read in the transaction; `MAX` never moves `last_used_at` back.
11. **The moved or resumed session.** The touch uses `grant.station_id` and `grant.session_id`. A session that is now on another station gets no touch.

## 5. Threat notes and risk class

**High.** The change makes grants live longer (authorization), and it changes the text of an authorization refusal. ADR-006 §5.3 changes.

| # | Threat | Defense |
| --- | --- | --- |
| T1 | The coordinator keeps its grant alive without work (a turn that sleeps or loops). | Accepted by the owner decision: a running turn is use. `expires_at` stays the hard cap (default 12 h, at most 7 days). The owner sees the session as Working and can revoke at any time. |
| T2 | A session, a worker or a board user fakes a running turn of the coordinator. | No API writes the read model or `last_used_at`. The run state comes only from the station's event stream of that session. Mail or a question to the coordinator starts real work, which is use. |
| T3 | A paired station claims that its sessions run. | The touch reads only the summary of `grant.station_id`, the station that hosts the session and holds the binding. No station can extend a grant of another station. A compromised station already holds the binding; `expires_at` caps the gain. |
| T4 | A stale read model extends a grant (link down, deck restart, replay not applied). | The freshness rule of §3.2 fails closed; no touch without both items. Both are read inside the transaction of the touch. |
| T5 | A turn revives an ended grant. | The touch needs `in_effect` at the touch time and `state = 'active'` in the `UPDATE`. Revoked, expired, idle-past-TTL and past-lifetime grants never change. Renew stays the only revival. |
| T6 | A race between the touch and an idle check or a revoke. | One database thread. The touch and `end_stale` share one transaction. A revoke in another transaction sets `state = 'revoked'`, and the `UPDATE` of the touch matches only `active`. A touch only makes `last_used_at` newer. The publication of a link stream and its touch run in one database closure, so no idle check runs between them (R2-2). |
| T7 | The refusal text leaks data. | It names only the end reason and the idle time of the newest grant of a session of the calling station, to that station. No grant id, no other station. A session without a grant gets the generic text. |
| T8 | The refusal hook admits a request or blocks the link. | The hook returns only `Option<String>`, and `explain` changes only `message`, only for `forbidden`. It reads an in-memory map under a read lock and fields of the parsed params; no I/O. |
| T9 | The sweep and the fold cost more. | The sweep: one `SELECT` of the active grants (already done), plus one row read and one summary parse for each active bound grant of a fresh station, every 30 s. Each batch, snapshot and `station.info` answer: one `SELECT` of the active grants of that station (index `tokens_by_caller`), plus one row read and one summary parse for each bound one. A station has few grants. |
| T10 | A lost handoff keeps a worker credential. | Unbound grants get no touch (§3.4); the idle-from-creation cleanup stays. |
| T11 | The answer of an ended link makes the read model look fresh, and the earlier link's running summary extends the grant before the new link replays a completion (R2-1). | The stream is kept by link token, recorded only while `link_features` holds that token, and forgotten when the link ends (§3.2). The touch reads the stream of the newest live link inside its transaction. `expires_at` stays the cap in all cases. |
| T12 | The cached refusal text is out of date (an idle end after the lifetime passed; a revoke of an ended grant) (R2-3). | The lifetime is classified at each refusal from the cached `expires_at` (`EndFacts::at`). Every write that changes an ended grant notes the grant's station, so the cache is read again (§3.6). The cache only selects a text: the link refusal and its code `forbidden` do not depend on it, and it never admits a request. |

## 6. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. A worker compiles only (`cargo check -p <crate> --tests`) and keeps each file under 800 lines (`wc -l`). The tests run at the merge into `v2` (DL-520).

T1 and T2 own different files and can run in parallel. T3 needs T2 (`GrantEnd`, `EndFacts`, `grant_ends`) and T1 (the checklist row names its tests). Revision 2 moves the `grant_end` declaration from `store/mod.rs` to `store/grants.rs`, so `store/mod.rs` has one owner (T1).

### T1 (backend): a running turn is use of a bound grant (≈ 190 lines of code, ≈ 420 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/grant_scopes.rs`
  - `crates/fleetdeck-deck/src/store/grant_turns.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_turns_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/read_model.rs`
  - `crates/fleetdeck-deck/src/store/mod.rs` (the field `link_streams`, its initializer, and the last line of `clear_link_features`; nothing else)
  - `crates/fleetdeck-deck/src/store/station_link.rs` (the `impl Inner` block of §3.3)
  - `crates/fleetdeck-deck/src/link/info.rs` (`fetch_info` gets `token: u64` and records the stream in its `ask` closure; `info_loop` does not change)
  - `crates/fleetdeck-deck/src/link/mod.rs` (line 207 `_live` → `live`; line 216 passes `live.token`; nothing else)
  - `docs/v2/adr/ADR-006-coordinator-mcp.md` (the §5.3 amendment of §3.8, one sentence at line 84, with "G6, DL-522")
- **Changes:** §3.1 (`turn_running`), §3.2 (the link stream: type, field, `set_link_stream`, `forget_link_stream`, `current_stream`, the record in `fetch_info`) and §3.3 (the module, touch points 1-4: the sweep, the event fold, the snapshot, `record_link_stream`). Update the module doc of `grant_scopes.rs` (lines 17-19, the sweep bullet): the sweep also records running turns as use. Update the module doc of `station_link.rs`: it also keeps the `station.info` stream of each live link (G6).
- **Tests** (`grant_turns_tests.rs`; own helpers: an in-memory `DeckStore` with `Det`, one or two pinned stations, a snapshot with `SessionSummary::new(..)` plus `current_run`/`state`, a link `up(token)` helper = `set_reachable(id, true)` + `set_link_features(id, token, vec![])`, a link `down(token)` helper = `clear_link_features(id, token)` + `set_reachable(id, false)`, `record_link_stream(id, token, (inc, head))` with the incarnation and head of the read model, a grant from `create_grant`, a binding through a `coordinator.bound` event like `grant_scopes_tests.rs:93-108`):
  1. `a_running_turn_keeps_a_bound_grant_in_effect_past_its_idle_time`: idle 60 s; running; advance 50 s, sweep: `last_used_at == now`; advance 50 s, sweep: still `active`.
  2. `the_idle_clock_starts_at_the_last_moment_the_turn_was_seen`: after test 1, `apply_events` with `run.completed`: `last_used_at` unchanged; advance 59 s, sweep: `active`; advance 2 s, sweep: `expired`/`idle`, one `coordinator.grant_expired` audit row with detail `idle`.
  3. `a_run_started_event_touches_the_grant_in_its_own_batch`: summary without a run; advance 59 s; `apply_events` with `run.started`: `last_used_at == now` without a sweep.
  4. `no_extension_without_a_live_link`: running, stream recorded, then `down(token)`; advance 61 s, sweep: `expired`/`idle`; `current_stream` is `None`.
  5. `a_delayed_answer_of_an_ended_link_does_not_make_the_new_link_fresh` (R2-1 regression): `up(1)`; the read model at head 5 with the session running; `down(1)`; `up(2)`; `record_link_stream(id, 1, (inc, 5))` returns `Ok(0)` and records nothing; advance 61 s, sweep: no touch, `expired`/`idle`. A second case: `record_link_stream(id, 2, (inc, 7))` with the cursor at 5: no touch; then `apply_events` up to 7 whose last event is `run.completed` of the session: no touch, the grant ends after its idle time.
  6. `no_extension_before_the_read_model_reaches_the_reported_head_or_with_another_incarnation`: stream head above the cursor: no touch; stream with incarnation `inc-2`: no touch.
  7. `no_extension_for_an_exited_or_lost_session_or_for_background_work_only`: `Exited` with a run, `Lost` with a run, and no run with `background_tasks = ["t1"]`: none touched.
  8. `a_turn_never_revives_a_grant_that_is_idle_past_its_time_past_its_lifetime_or_revoked`: each with a running turn; after the sweep and after `record_link_stream`: `expired`/`idle`, `expired`/`expired`, `revoked` with `last_used_at` unchanged.
  9. `an_unbound_grant_gets_no_extension`: an unbound coordinator grant keeps `last_used_at == None`; an unbound worker grant (made like `worker_grant_tests.rs` makes one) ends `idle` from its creation.
  10. `a_bound_worker_grant_is_extended`.
  11. `the_turn_of_another_session_or_another_station_does_not_extend_a_grant`.
  12. `a_query_after_a_long_silent_turn_is_admitted`: idle 60 s; running; advance 50 s, sweep, advance 50 s, sweep; `coordinator_query` `ListSessions` with the binding: accepted.
  13. `a_reconnect_just_before_the_idle_deadline_keeps_the_grant` (R2-2 regression): idle 60 s; bound at t0; the session running in the read model; no live link; advance 59 s; `up(1)`; `record_link_stream(id, 1, (inc, cursor))` returns `Ok(1)` and `last_used_at == now`; advance 2 s; `coordinator_query` `ListSessions` with the binding, without a sweep: accepted.
  14. `a_snapshot_that_makes_the_station_fresh_touches_at_once` (R2-2): idle 60 s; `up(1)`; stream head 9 above the cursor; advance 59 s; `apply_snapshot` at watermark 9 with the session running: `last_used_at == now`; advance 2 s; the query is accepted without a sweep.
  15. `a_batch_touches_a_running_session_without_an_event_in_it` (R2-2): sessions `coord-1` (running, bound grant) and `w-1`; stream head above the cursor; advance 59 s; `apply_events` with events of `w-1` only, up to the head: the grant of `coord-1` has `last_used_at == now`.
  16. `the_newest_live_link_that_answered_decides`: `up(1)` with stream head 5 (caught up); `up(2)` without a stream: `current_stream` is the stream of link 1; `record_link_stream(id, 2, (inc, 9))` with the cursor at 5: `current_stream` is the stream of link 2 and there is no touch; `down(2)`: the stream of link 1 decides again; `down(1)`: `None`.
- **depends_on:** none.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_turns) | test(grant_scopes) | test(station_link) | test(coordinator) | test(worker_grant) | test(owner_grants) | test(read_model) | test(link::) | binary(grant_stations) | binary(worker_grant) | binary(live_grant_routes) | binary(deck_link)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - `wc -l crates/fleetdeck-deck/src/store/mod.rs` is below 800.

### T2 (backend): the store refusals say why a grant ended (≈ 110 lines of code, ≈ 150 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/grants.rs` (the `#[path]` declaration of `grant_end`, §3.5; nothing else)
  - `crates/fleetdeck-deck/src/store/grant_end.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_end_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/coordinator.rs`
  - `crates/fleetdeck-deck/src/store/coordinator_tests.rs`
  - `crates/fleetdeck-deck/src/store/coordinator_query.rs`
  - `crates/fleetdeck-deck/src/store/grant_scopes_tests.rs`
  - `crates/fleetdeck-deck/tests/grant_stations.rs`
- **Changes:** §3.5 (the module with `GrantEnd`, `EndFacts`, `grant_end`, `end_text`, `grant_refusal`, `station_grant_ends`, `grant_ends`; the `decide_coordinator` and `check` texts; the module doc of `coordinator.rs`). Change the old text assertions of §3.8.
- **Tests:**
  - `grant_end_tests.rs`:
    1. `grant_end_orders_revoked_then_lifetime_then_idle`: revoked → `Revoked`; `expired`/`idle` in its lifetime → `Idle`; `expired`/`idle` past its lifetime → `Lifetime`; `expired`/`expired` → `Lifetime`; `active` past `expires_at` → `Lifetime`; `active` idle past its TTL → `Idle`; `active` in effect → `None`; unbound coordinator grant → `None`.
    2. `end_text_says_why_and_what_the_owner_can_do`: the three exact texts of §3.5 and the five duration examples.
    3. `station_grant_ends_keeps_the_newest_grant_of_each_session_of_the_station`: `coord-1` with an older revoked and a newer idle-expired grant → `EndFacts { end: Idle, expires_at }` with the `expires_at` of the newer grant; `coord-2` with a grant in effect → absent; a grant of another station → absent.
    4. `end_facts_classify_the_lifetime_at_the_refusal_time`: `Idle` at `expires_at` → `Idle`; `Idle` at `expires_at + 1` → `Lifetime`; `Revoked` at `expires_at + 1` → `Revoked`; `Lifetime` before `expires_at` → `Lifetime`.
  - `coordinator_tests.rs`: line 274 expects `"Your fleet grant expired (idle for 1 second). The owner can renew it on the board."`; new test `a_refused_call_of_an_ended_grant_says_why`: lifetime ended (another grant keeps the station covered) → the lifetime text; revoked → the revoked text; the only grant idle and the station no longer covered → the idle text, not `NOT_COVERED`.
  - `grant_scopes_tests.rs`: line 413 expects the idle text with "1 second"; new test `a_refused_query_of_an_ended_grant_says_why` (lifetime and revoked through `coordinator_query`).
  - `tests/grant_stations.rs`: `assert_not_covered` becomes `assert_refused(receipt, text)`; the lapse test expects the lifetime text, the idle test the idle text with "1 second"; the query test (`:493-497`) the lifetime text. `:222` and `:563` stay.
- **depends_on:** none.
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_end) | test(coordinator) | test(grant_scopes) | binary(grant_stations) | binary(coordinator_fleet) | binary(worker_grant)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1).
  - `grep -rn 'expired after its idle time\|grant is revoked or expired' crates/` prints nothing.

### T3 (backend): the link refusal says why (≈ 100 lines of code, ≈ 300 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-link/src/grants.rs`
  - `crates/fleetdeck-link/src/control.rs` (line 729 only)
  - `crates/fleetdeck-link/src/control_tests.rs` (one `#[path]` module pair at the end, like `control_rx_tests.rs` at lines 697-701)
  - `crates/fleetdeck-link/src/control_refusal_tests.rs` (new)
  - `crates/fleetdeck-deck/src/peers.rs`
  - `crates/fleetdeck-deck/src/peers_tests.rs` (new, declared at the end of `peers.rs`)
  - `crates/fleetdeck-deck/tests/grant_refusal_text.rs` (new, with `mod common;`)
  - `crates/fleetdeck-deck/src/store/worker_grant.rs` (the last line of `revoke_tx` only, §3.6; the file must stay below 800 lines)
  - `crates/fleetdeck-deck/src/store/grants_lifecycle.rs` (`ended_station`, `apply_ended`, the `expired` branch of `owner_ended`, the `snapshot_revoked` write of `apply_projection`, §3.6)
  - `crates/fleetdeck-deck/src/store/grants_lifecycle_notes_tests.rs` (new, declared at the end of `grants_lifecycle.rs` with `#[cfg(test)] #[path = "grants_lifecycle_notes_tests.rs"] mod notes_tests;`)
  - `docs/v2/SECURITY-CHECKLIST.md` (one row `522` after the last row: G6, the files of T1-T3, threats T1-T12 of §5 in short, the test names of T1 and T3)
- **Changes:** §3.6 (the trait method, `explain`, `control.rs:729`, the `ends` cache of `DeckPeers` with `EndFacts`, `refusal` with `EndFacts::at(self.store.now_ms())`, and the notes of the ended-grant writes).
- **Tests:**
  - `control_refusal_tests.rs` (a fake `GrantSource` with no scopes, a call counter, and `refusal` that returns `Some(format!("why {method} {caller}"))` with `caller = params["caller"]`; build the channel like `side()` in `control_tests.rs:81-86` with this source):
    1. `a_scope_refusal_takes_the_text_of_the_grant_source`: `coordinator.query` with `{"caller": "c1"}` → `Outcome::Err` code `forbidden`, message `why coordinator.query c1`.
    2. `a_source_without_a_text_keeps_the_generic_refusal`: `refusal` returns `None` → `` `coordinator.query` needs scope coordinate, which the pairing did not grant ``.
    3. `the_source_is_asked_only_for_a_scope_refusal`: a wrong-role method keeps its `wrong_role` text and the counter stays 0.
  - `peers_tests.rs`:
    1. `load_fills_the_end_texts_from_the_store`: an idle-expired bound grant of `coord-1`; `DeckPeers::load`; `refusal(fp, "coordinator.query", {"caller":"coord-1"})` is the idle text.
    2. `refusal_answers_only_for_the_calling_station_its_sessions_and_the_two_methods`: another fingerprint → `None`; `coordinator.authority_check` → `None`; params without a caller → `None`; `coordinator.call` reads `call.caller`.
    3. `a_revoked_key_has_no_end_texts_and_a_rotated_key_keeps_them`.
    4. `an_idle_end_becomes_the_lifetime_text_when_the_lifetime_passes` (R2-3): the sole grant of `coord-1` expired idle; `load`; the idle text; advance the clock past `expires_at` without a refresh: the lifetime text.
    5. `a_revoke_of_an_idle_expired_grant_changes_the_text` (R2-3): the sole grant expired idle (the station has no `coordinate`); `load`; `revoke_grant`; `refresh_changed()`: the revoked text; the stored scopes of the station still have no `coordinate`.
  - `grants_lifecycle_notes_tests.rs` (R2-3):
    1. `a_revoke_of_an_ended_grant_notes_its_station`: `revoke_tx` of an idle-expired grant returns a set with `grant.station_id`; a revoke of an already revoked grant returns no extra station.
    2. `a_coordinator_ended_for_an_ended_grant_notes_its_station`: `apply_events` with `coordinator.ended` (reason `revoked`) for an idle-expired grant: `take_scope_changes()` has the station.
    3. `a_snapshot_that_revokes_an_ended_unbound_grant_notes_its_station`.
  - `tests/grant_refusal_text.rs` (the fixture of `common`, `f.connect()`, `common::fleet::bound_edit` and `common::fleet::query`):
    1. `a_link_refused_query_of_an_idle_expired_grant_says_why`: bound grant of the fixture station, idle 1 s; advance 2 s; `sweep_grants`; `peers.refresh_changed()`; `coordinator.query` over `station.control` → `forbidden` with the idle text ("1 second").
    2. `a_link_refused_call_of_a_lifetime_ended_or_revoked_grant_says_why`: `coordinator.call` with a command id → the lifetime text; after `revoke_grant` and a refresh → the revoked text.
    3. `a_caller_without_a_grant_keeps_the_generic_text`.
    4. `a_link_refused_query_after_an_idle_end_and_then_the_lifetime_says_the_lifetime_ended` (R2-3): idle 1 s, lifetime 10 s; advance 2 s, sweep, refresh: the idle text; advance 10 s, sweep (no scope change): the lifetime text.
- **depends_on:** T2 (and T1 for the checklist row).
- **Acceptance (at the merge):**
  - `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(control) | test(methods)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(peers) | test(grants_lifecycle) | test(worker_grant) | test(owner_grants) | test(grant_scopes) | binary(grant_refusal_text) | binary(grant_stations) | binary(worker_grant) | binary(deck_link)'`
  - `cargo clippy -p fleetdeck-link -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1). `wc -l crates/fleetdeck-link/src/control.rs crates/fleetdeck-deck/src/store/worker_grant.rs` shows each below 800.

### At the merge (coordinator)

- `make check` once, after T3.
- The adversary PRE-PR REVIEW of the final branch (high risk), with the acceptance commands above as its fast checks.

## 7. Decisions made in this draft

1. **A running turn is `current_run` set and the process alive** (§3.1). Rejected: the card status Working (it includes background work, which the owner decision does not name); `current_turn` (a run can sit between turns).
2. **The extension is recorded as use (`last_used_at`), not computed at each check.** It survives a deck restart, and no idle check changes. Rejected: a predicate inside `in_effect` (it is pure and used in about ten places; it cannot read the read model); a new column `turn_seen_at` (a migration for no gain).
3. **Four touch points, each in the database step that can make the station fresh: the sweep (30 s), each event batch (the whole station), each snapshot, and each `station.info` answer of a live link** (§3.3, revision 2). The batch closes the gap at the start of a turn; the snapshot and the answer close the gap at a reconnect (R2-2). Rejected: the sweep only (a turn that starts in the last 30 s of the idle time loses its grant); the sweep and the batch of the session only (revision 1: a reconnect near the idle deadline loses the grant before the next sweep); a touch at the end of a turn (a replay after a disconnect would date it to the replay); a grace period in the idle check (it changes every idle check).
4. **Freshness is "the newest live link answered `station.info` + the read model reached the head of that answer"**, with the answer kept by link token (§3.2, revision 2). Rejected: reachability only (the read model of the old link can say "running" until the replay lands); `SnapshotMark` (a plain replay never sets it); the time of the answer compared with `linked_since` (revision 1: a late answer of an ended link is stamped after the new link came up, R2-1); a token check in `info_loop` before it publishes (a check and a later publish can still race with the end of the link; the lock order of `set_link_stream` cannot).
5. **No revival:** the touch needs the grant in effect at the touch time.
6. **Every bound grant, of both kinds, gets the extension; unbound grants do not** (§3.4).
7. **The clock and the link stream are read in the transaction of the touch,** also in `sweep_grants` (the GS2 rule of `grant_scopes.rs:11-16`). Nothing captured before the database call decides a touch.
8. **The text comes from the deck at both refusal points:** the store checks, and a new provided method `GrantSource::refusal` at the link scope check. Rejected: a station-side rewrite (the station does not know the reason); keeping `coordinate` for a station whose grant ended (weaker authorization); a new reject code (an older station cannot decode an unknown `RejectCode`).
9. **The code stays `forbidden`;** only the message changes.
10. **Three texts** (idle, lifetime, revoked); the idle text names the idle time of the grant, in whole minutes or in seconds.
11. **The ends live in memory beside the link scopes as `EndFacts` (the end and `expires_at`)**; the lifetime is classified at each refusal, and every write that changes an ended grant notes its station (§3.6, revision 2, R2-3). Rejected: a database read in the link read loop (it must not block); a cached `GrantEnd` refreshed only with scope changes (revision 1: it keeps "renew" after the lifetime ended, and misses a revoke of an ended grant); a periodic refresh of every station (it bounds the error but does not remove it).
12. **The desktop tools, the outbox and `fleet_view` stay unchanged** (§3.9).
13. **Risk class high:** the adversary does the DESIGN REVIEW and the PRE-PR REVIEW.
14. **The `grant_end` module is a child of `store/grants.rs`,** so `store/mod.rs` has one owner (T1) and T1 and T2 can run in parallel (revision 2).

## 8. Owner questions

1. **Does background work count?** The backlog case is "its own long workflow only waits". A Claude Code workflow or a background subagent runs after its turn ended; the board shows that session as Working, but no turn runs. Under the decision of 2026-10-24 the grant can still idle out then. Option: count agent work, `current_run.is_some() || running_agents > 0` (a running workflow or subagent), but not a background shell task (a dev server would keep the grant alive). **Default: no (the decision as written).** The change is one line in `turn_running` and one test.
2. **Do bound worker grants (MCP P4a) get the extension?** **Default: yes** (§3.4).

## Decision log entries (to append)

## DL-522: G6 a running turn of the session of a bound grant counts as use (owner decision 2026-10-24; design agent, 2026-10-24)

- Context: B24 G6; design `docs/v2/design/grant-idle.md` §3.1-§3.4. A coordinator grant went idle after 30 min while its session worked without fleet calls.
- Decision: the deck moves `last_used_at` of a bound grant in effect to the deck time while its session has a running turn (`current_run` set, process not exited or lost) in a fresh read model. The sweep does it every 30 s, before `end_stale` in the same transaction. Each event batch and each snapshot does it for every grant of the station in its own transaction, and each `station.info` answer of a live link does it in the same database closure that records the answer (revision 2). The idle clock starts at the last moment the deck saw the turn. `expires_at` stays the hard cap. A touch never revives a grant that is not in effect. The rule applies to every bound grant: spawn-time, owner-given (P4b) and worker (P4a). No schema change. ADR-006 §5.3 is amended.
- Rejected: an idle predicate that reads the read model at each check; a new column; a touch at the end of a turn.

## DL-523: G6 a station read model is fresh when its newest live link answered `station.info` and the cursor reached the head of that answer (design agent, 2026-10-24; revision 2)

- Decision: the deck keeps the incarnation and head of each `station.info` answer by link token (`link_streams`). It records an answer only while `link_features` holds the token, and it forgets the entry when the link ends (lock order `link_features`, then `link_streams`). The touch of DL-522 reads, inside its transaction, the stream of the newest live link and needs `stations.incarnation_id == incarnation` and `stations.cursor >= head`. Else no touch (fail closed): link down, deck restart, replay or snapshot not applied, an answer of an ended link.
- Rejected: reachability only; `SnapshotMark` (a replay without a snapshot never sets it); the time of the answer compared with `linked_since` (a late answer of an ended link passes it).

## DL-524: G6 a refused call of a grant that ended says why (design agent, 2026-10-24)

- Decision: `store/grant_end.rs` gives `GrantEnd` (idle, lifetime, revoked) and three texts: "Your fleet grant expired (idle for N minutes). The owner can renew it on the board.", "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.", "Your fleet grant was revoked. The owner can give a new grant on the board." The store checks of `decide_coordinator` and `coordinator.query` use them for a caller with a grant. The link scope check asks the new provided method `GrantSource::refusal`; `DeckPeers` answers from an in-memory map of `EndFacts` (the end and `expires_at`). It classifies the lifetime at each refusal. It refreshes the map with the scopes, and every write that changes an ended grant notes the grant's station (revision 2). The code stays `forbidden`. A worker call, `authority_check` and the desktop tools keep their texts.
- Rejected: a station-side rewrite; keeping `coordinate` for a station whose grant ended; a new reject code.

## DL-525: G6 is high risk: adversary design review and pre-PR review (design agent, 2026-10-24)

- Decision: G6 changes how long a grant authorizes calls and the text of an authorization refusal. It gets the adversary DESIGN REVIEW before T1-T3 start and the PRE-PR REVIEW of the final branch. `SECURITY-CHECKLIST.md` gets row 522.

## Revision 2 (design review)

The adversary DESIGN REVIEW round 1 (`~/fleetdeck-briefs/backlog/grant-idle/adv-design.md`, verdict FAIL, critic Codex) gave three findings. Each was checked against the code at `c4fbdbfd`; all three are correct. The owner decision stays binding: a running turn counts as use, and `expires_at` stays the hard cap.

| # | Finding | Verified | Change |
| --- | --- | --- | --- |
| R2-1 | BLOCKER (0.96): a publication time does not identify the answering link. A late `station.info` answer of an ended link can be stamped after the new `linked_since`, and the running summary and cursor of the earlier link then pass `caught_up` before the new link replays a completion. | Yes. `record_station_info` awaits `remember_machine` before `set_station_info` stamps the time (`station_machines.rs:19-22`, `store/mod.rs:622`); `info_loop` does not check its link (`link/info.rs:63-95`). | §1.3 items 5-7 (facts). §3.2: the stream is kept by link token in `link_streams`; `set_link_stream` records it only while `link_features` holds the token; `clear_link_features` forgets it; the touch reads the stream of the newest live link inside its transaction. `fetch_info` gets the token. §4 items 5 and 7, §5 T11, §7 decisions 4 and 7, DL-523. T1 owns `store/mod.rs`, `station_link.rs`, `link/info.rs` and two lines of `link/mod.rs`. Regression test T1-5 (a delayed answer of an ended link across a reconnect) and T1-16. |
| R2-2 | MAJOR (0.99): a fresh running snapshot can lose the race to idle expiry. After a reconnect near the idle deadline, a query before the next sweep fails the idle check, and the sweep cannot repair it (no revival). | Yes. Revision 1 touched only at the sweep and for sessions with an event in a batch; `apply_snapshot` and the `station.info` answer did not touch; a duplicate binding does not refresh `last_used_at` (`grants_lifecycle.rs:203-221`). | §3.3 touch points 2-4: every batch and every snapshot touch every grant of the station in their transaction; `record_link_stream` records the answer and touches in one database closure, so no idle check runs between them (§1.3 item 7). The batch keeps no set of running sessions. §4 items 2, 4 and 8, §5 T6 and T9, §7 decision 3, DL-522. Regression tests T1-13 (reconnect 1 s before the idle deadline, then a query 2 s later without a sweep), T1-14 and T1-15. No revival is added: the touch still needs the grant in effect. |
| R2-3 | MAJOR (1.0): scope changes cannot keep the cached end reasons current. After an idle end, a later lifetime end or a revoke of the ended grant changes no scope, so the link keeps advising a renewal. `GrantEnd` has no `expires_at`. | Yes. `end_stale` visits only active grants; `revoke_tx` revokes an expired grant (`state != 'revoked'`, `worker_grant.rs:702-703`) and returns only the scope changes; `apply_ended` returns an empty set (`grants_lifecycle.rs:270-277`). | §3.5: `EndFacts { end, expires_at }` with `EndFacts::at(now)`; `station_grant_ends` and `grant_ends` return it. §3.6: `DeckPeers::refusal` classifies the lifetime at each refusal with the deck clock; `revoke_tx`, `apply_ended`, the `expired` branch of `owner_ended` and the `snapshot_revoked` write of `apply_projection` return the grant's station, so the station is noted and its ends are read again. §3.8, §5 T12, §7 decision 11, DL-524. T3 owns `worker_grant.rs` (one line) and `grants_lifecycle.rs`. Tests T2 `grant_end_tests` 4, T3 `peers_tests` 4-5, `grants_lifecycle_notes_tests` 1-3, `grant_refusal_text` 4. |

Other changes of revision 2:

- The `grant_end` declaration moves from `store/mod.rs` to `store/grants.rs` (§3.5, §7 decision 14), so T1 and T2 do not share a file.
- The estimates of T1 and T3 grow (≈ 190 and ≈ 100 lines of code).
- No owner question changes. No new schema, wire type or dependency.

## Final task list (revision 2)

| Id | Title | Depends on |
| --- | --- | --- |
| T1 | A running turn is use of a bound grant: the link stream by token, four touch points (sweep, batch, snapshot, `station.info` answer) | none |
| T2 | The store refusals say why a grant ended: `grant_end` with `GrantEnd`, `EndFacts` and the three texts | none |
| T3 | The link refusal says why: `GrantSource::refusal`, the `EndFacts` cache of `DeckPeers`, and station notes for ended-grant writes | T2 (code), T1 (the checklist row) |
