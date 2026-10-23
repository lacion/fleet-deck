# "Last seen" of an offline station is the end of its last link (B22)

- **Status:** Draft for the architect, revision 1 (2026-10-23). Risk: normal (§4). One Codex review of the final branch.
- **Owner report (BACKLOG B22):** for an offline station, "Last seen" shows the time when its last link started, not the time when the link ended. Wanted: the last moment the station was reachable.
- **Code base:** branch `v2`, HEAD `497b2fbc`. All file:line references are on this commit.
- **Evidence:** `/home/luismorales/fleetdeck-briefs/backlog/last-seen/evidence-live-deck.txt` (the owner's live deck on Lighting, read only).

## 1. The problem and the verified facts

### 1.1 Where the board reads the value

1. `web/src/board/StationsPanel.tsx:169-173` computes the two link facts of a station card:
   ```tsx
   const up = freshness !== 'stale' && station.reachable;
   // `last_seen` is the time the last link came up: for a station that is up it is the
   // age of the link (the Linked fact in Details), never a "Last seen" (principle 7).
   const lastSeen = up ? null : station.last_seen;
   const linkedAt = up ? station.last_seen : null;
   ```
2. `StationsPanel.tsx:206-208` shows `Last seen {agoText(lastSeen, now)}` with the ISO time as the tooltip. `StationsPanel.tsx:360-367` shows the Details fact `Linked` with `agoText(linkedAt, now)`.
3. `web/src/board/StationSummary.tsx:1-2` is only a comment that points to this rule. It does not read `last_seen` (the backlog names it as the place of `showLastSeen`; no symbol `showLastSeen` exists on HEAD).
4. The board uses one field, `last_seen`, for two different facts: the age of the live link (online) and the last contact (offline). The code comment states that the value is the link start. So the online case is correct and the offline case is the B22 bug.

### 1.2 Where the deck writes the value

1. The value is the column `stations.last_seen INTEGER` (`crates/fleetdeck-deck/src/store/schema.rs:41`, table at `schema.rs:31-45`). It is read into `StationRecord.last_seen` (`crates/fleetdeck-deck/src/store/stations.rs:59`, `:100-116`, `:141`).
2. The one writer is `DeckStore::touch_station` (`stations.rs:430-446`): `UPDATE stations SET last_seen = ?2 WHERE station_id = ?1` with the deck clock, then `board_changed()`.
3. The one caller is `link::serve` (`crates/fleetdeck-deck/src/link/mod.rs:206-209`), right after `ctx.up(...)`, when a link comes up. Nothing writes `last_seen` while the link lives or when it ends.
4. When the last link of a station ends, `LiveGuard::drop` (`link/mod.rs:170-182`) only calls `set_reachable(&id, false)`. Reachability is memory only: `reachable: Mutex<BTreeSet<StationId>>` (`crates/fleetdeck-deck/src/store/mod.rs:257-258`), `set_reachable` (`store/mod.rs:591-604`), `is_reachable` (`store/mod.rs:700-711`). `peers.rs:200` also clears it when a station is removed.
5. `serve` returns `control.closed().await` (`link/mod.rs:271`). The deck logs "station link up" (`link/mod.rs:210`) but no line when a link ends.

### 1.3 What the API sends

1. `StationView.last_seen: Option<i64>` (`crates/fleetdeck-proto/src/board.rs:208-209`, doc "Unix ms of the last link."), built by `local::station_view` (`crates/fleetdeck-deck/src/local.rs:112-133`) as `last_seen: st.last_seen`.
2. `station_view` feeds `GET /api/stations` (`http/read_routes.rs:57-61`), the station routes (`http/station_routes.rs:158`, `:176`), the board WebSocket snapshot and deltas (`http/ws.rs:251-255`, `:463-479`), the wall display view (`http/ws_display.rs:43-47`, `http/display_view.rs:146-158` keeps `last_seen`) and the member view (`http/read_routes.rs:20-33` keeps `last_seen`). `fleetdeck deck stations --json` prints the same struct (`crates/fleetdeck-cli/src/deck.rs:100`); its text form does not show the time (`deck.rs:111-123`).
3. No other reader: the MCP `fleet_view`, the fleet summary and the CLI text output do not use `last_seen` (search on HEAD). `DisplayView.last_seen` (`proto/src/display.rs:33`) is a different value (a wall display, not a station).

### 1.4 The link heartbeat

1. The control channel sends a heartbeat `ping` every 15 s (`HEARTBEAT_INTERVAL`, `crates/fleetdeck-link/src/control.rs:40-41`) and ends a link after 45 s without a frame (`IDLE_LIMIT`, `control.rs:43`, check at `control.rs:507`, `:539-545`).
2. Each message read from the peer refreshes `Shared::last_rx` (`control.rs:146`, `:192-198`, the read loop at `control.rs:648`). The value is private: the deck cannot read it.
3. So a link that dies on the network (sleep, Wi-Fi loss) ends up to 45 s after the last frame. A link that the station closes ends at once.

### 1.5 Live evidence (Lighting, 2026-10-23, read only)

`stations.last_seen` of the live deck equals the "station link up" log time of each station, to the millisecond (the write comes 2-3 ms before the log line):

| Station | `last_seen` (deck.db) | "station link up" (journal) |
| --- | --- | --- |
| Lighting | 15:49:30.409Z | 15:49:30.412Z |
| thunder | 16:10:19.760Z | 16:10:19.762Z |
| dev1 | 16:21:15.087Z | 16:21:15.090Z |
| mac-m4 | 16:22:52.596Z | 16:22:52.598Z |

The journal has no line for the end of a link (count 0 since 00:00). This confirms the backlog observation.

### 1.6 Corrections to the backlog

1. "`showLastSeen`, `web/src/board/StationSummary.tsx`": no such symbol. The rule is in `StationsPanel.tsx:169-173`; `StationSummary.tsx` has only a comment.
2. "keep the start for the status history": the deck has no station status history. The start is used only by the Linked fact of an online station. It needs no storage (§2 Q2).
3. A second case has the same bug: while the board status is stale (`freshness === 'stale'`), a station last reported online also shows "Last seen" with the link start (`up` is false, so `lastSeen = station.last_seen`). §3.6 fixes it too.

## 2. Answers to the questions of the backlog

**Q1. Does the deck store the link start and the link end separately?**
No. It stores one time, `stations.last_seen`, and writes it only at link start (§1.2). It stores no link end and no "last heard" time.

**Q2. If not, add the link end when the link closes, and keep the start?**
Yes, with two changes to the plan of the backlog:
1. The column `last_seen` gets the meaning that its name has: the last moment the deck heard the station. The deck writes it at link start, every 30 s while the link lives, and when a link ends. A write only at link close is not sufficient: when the deck process stops (a restart, a crash, a power loss), no close runs. The deck shutdown does not wait for the link tasks (`link/runtime.rs:438-446`, the `serve` tasks are spawned and not kept, `runtime.rs:309`, `:461`). With the refresh, the error after a deck stop is at most 30 s.
2. The time of the link end is the time of the last frame from the station, not the time when the deck noticed the end. After a network loss the deck notices the end up to 45 s late (§1.4). A new read-only accessor `Control::since_last_frame()` gives the idle time.
3. The start of the link is kept in memory, beside the reachability, and sent as a new optional field `linked_at`. A link always belongs to the running deck process, so memory holds the start for every link that is up. No column and no migration are necessary.

## 3. Design

### 3.1 Semantics

| Field | Meaning | Source |
| --- | --- | --- |
| `StationView.last_seen` | Unix ms of the last frame that the deck received from the station, on the deck clock. `null`: the station never linked to this deck. | `stations.last_seen` (existing column, new meaning) |
| `StationView.linked_at` (new, optional) | Unix ms since which the station is reachable without a gap, in this deck process. Absent while it is not reachable. | memory (`DeckStore` reachability map) |

The stored `last_seen` never moves back (SQL `MAX`, §3.3). Two links of one station can overlap for a moment (a later link wins, NV-12): `MAX` makes the order of their writes unimportant.

### 3.2 `fleetdeck-link`: the idle time of a link

Add to `impl Control` in `crates/fleetdeck-link/src/control.rs` (after `outstanding`, near `control.rs:387-391`):

```rust
/// How long ago the last message came from the peer (a frame, a
/// WebSocket ping or pong). After the end of the link it keeps growing
/// from the last message.
#[must_use]
pub fn since_last_frame(&self) -> Duration {
    self.shared.last_rx().elapsed()
}
```

`last_rx` is set at the start of the channel (`control.rs:449`) and at each read (`control.rs:648`). No other change in the link crate. The station side does not use it.

### 3.3 Deck store: the link presence of a station

New module `crates/fleetdeck-deck/src/store/station_link.rs` (declared `mod station_link;` in `store/mod.rs`). It takes over `set_reachable`, `is_reachable` and `reachable_set` from `store/mod.rs` (that file is at 796 lines; the move gives about 25 lines back) and `touch_station` from `stations.rs`.

1. `store/mod.rs:257-258` changes to:
   ```rust
   /// The stations with a live link (§7.2 `unreachable`) and the Unix ms
   /// since which each one is reachable without a gap (B22 `linked_at`).
   reachable: Mutex<BTreeMap<StationId, i64>>,
   ```
   (`reachable: Mutex::default()` at `store/mod.rs:412` stays.)
2. `station_link.rs`:
   ```rust
   //! The link presence of the stations (B22, design `last-seen.md` §3.3):
   //! which stations have a live link and since when (memory of this deck
   //! process), and the last time the deck heard each one
   //! (`stations.last_seen`).

   use std::collections::BTreeMap;
   use std::collections::btree_map::Entry;
   use std::sync::{MutexGuard, PoisonError};

   use fleetdeck_proto::StationId;
   use rusqlite::params;

   use super::{DeckStore, Result, call};

   impl DeckStore {
       /// Records whether a link to `station` is up. The first `up` keeps
       /// the deck time as the start of the continuous link; a later `up`
       /// (a second link) keeps it; `down` forgets it. The board version
       /// grows only on a change.
       pub fn set_reachable(&self, station: &StationId, up: bool) {
           let now = self.now_ms();
           let changed = {
               let mut map = self.reachable_map();
               if up {
                   match map.entry(station.clone()) {
                       Entry::Vacant(e) => {
                           e.insert(now);
                           true
                       }
                       Entry::Occupied(_) => false,
                   }
               } else {
                   map.remove(station).is_some()
               }
           };
           if changed {
               self.board_changed();
           }
       }

       /// True while the link to `station` is up.
       #[must_use]
       pub fn is_reachable(&self, station: &StationId) -> bool {
           self.reachable_map().contains_key(station)
       }

       /// The Unix ms since which `station` is reachable without a gap;
       /// `None` while it is not reachable.
       #[must_use]
       pub fn linked_since(&self, station: &StationId) -> Option<i64> {
           self.reachable_map().get(station).copied()
       }

       /// Records that the deck heard `station` at `at` (Unix ms). The
       /// stored time never moves back. An unknown station is not an error.
       /// No board change: the board shows the time only for a station that
       /// is not reachable, and the end of the last link changes the
       /// reachability after this write (design §3.4).
       ///
       /// # Errors
       /// A database error.
       pub async fn station_heard(&self, id: StationId, at: i64) -> Result<()> {
           call(&self.inner.db, move |conn| {
               conn.execute(
                   "UPDATE stations SET last_seen = MAX(COALESCE(last_seen, ?2), ?2)
                    WHERE station_id = ?1",
                   params![id.as_str(), at],
               )?;
               Ok(())
           })
           .await
       }

       fn reachable_map(&self) -> MutexGuard<'_, BTreeMap<StationId, i64>> {
           self.inner
               .reachable
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
       }
   }

   #[cfg(test)]
   #[path = "station_link_tests.rs"]
   mod tests;
   ```
3. Remove `touch_station` (`stations.rs:430-446`). Its one caller changes (§3.4).
4. No schema change. The existing rows keep their value (the start of their last link) until the next write for that station.

### 3.4 Deck link: refresh and the end of a link

New module `crates/fleetdeck-deck/src/link/seen.rs` (declared `mod seen;` in `link/mod.rs`, which is at 709 lines), in the style of `link/info.rs`:

```rust
//! The last-seen time of a live station link (B22, design `last-seen.md`
//! §3.4): written at the start, every `SEEN_REFRESH` while the link lives,
//! and at its end, as the time of the last frame from the station.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;

use super::DeckLink;
use crate::store::DeckStore;

/// How often the deck writes the last-seen time of a live link.
pub(super) const SEEN_REFRESH: Duration = Duration::from_secs(30);

/// The deck time of the last frame: `now_ms` minus the idle time, never
/// below 0.
pub(super) fn heard_at_ms(now_ms: i64, idle: Duration) -> i64 {
    let idle = i64::try_from(idle.as_millis()).unwrap_or(i64::MAX);
    now_ms.saturating_sub(idle).max(0)
}

fn heard_at(store: &DeckStore, control: &Control) -> i64 {
    heard_at_ms(store.now_ms(), control.since_last_frame())
}

/// Starts the refresh of `id` for the life of `control`.
pub(super) fn keep_seen(ctx: &Arc<DeckLink>, control: &Control, id: &StationId) {
    let (store, control, id) = (ctx.store.clone(), control.clone(), id.clone());
    tokio::spawn(async move {
        let at = || heard_at(&store, &control);
        seen_loop(&store, &id, SEEN_REFRESH, at, control.closed()).await;
    });
}

/// Writes `at()` every `every` until `closed` resolves. The first write
/// comes after one interval (the link start wrote the time already).
pub(super) async fn seen_loop<F>(
    store: &DeckStore,
    id: &StationId,
    every: Duration,
    mut at: F,
    closed: impl Future,
) where
    F: FnMut() -> i64,
{
    tokio::pin!(closed);
    loop {
        tokio::select! {
            _ = &mut closed => return,
            () = tokio::time::sleep(every) => {}
        }
        if let Err(e) = store.station_heard(id.clone(), at()).await {
            tracing::warn!(error = %e, station = %id, "station last_seen");
        }
    }
}

/// The last write of a link that ended. `serve` awaits it before the
/// guard of the link marks the station unreachable.
pub(super) async fn record_end(store: &DeckStore, id: &StationId, control: &Control) {
    if let Err(e) = store.station_heard(id.clone(), heard_at(store, control)).await {
        tracing::warn!(error = %e, station = %id, "station last_seen");
    }
}

#[cfg(test)]
#[path = "seen_tests.rs"]
mod tests;
```

Changes in `link::serve` (`link/mod.rs:184-272`):

1. `link/mod.rs:207-209`: replace `ctx.store.touch_station(id.clone())` with `ctx.store.station_heard(id.clone(), ctx.store.now_ms())` (same warning text "station last_seen").
2. After `fetch_info(&ctx, &control, &id, &station.name);` (`link/mod.rs:211`): add `seen::keep_seen(&ctx, &control, &id);`.
3. Replace the last line `control.closed().await` (`link/mod.rs:271`) with:
   ```rust
   let closing = control.closed().await;
   seen::record_end(&ctx.store, &sync.station, &control).await;
   tracing::info!(station = %station.name, reason = %closing.reason, "station link ended");
   closing
   ```
   `_live` (the `LiveGuard`) drops when `serve` returns, so the order is: the last write, then `set_reachable(false)`, then `board_changed()`. The board delta that shows the station offline reads the new `last_seen`.

Board traffic: the refresh and the end write do not call `board_changed()`. The board WebSocket sends a station list only when its `view()` differs (`http/ws.rs:463-479`): a refreshed `last_seen` goes out with the next real board change. That is enough, because the board does not show `last_seen` for a station that it sees online (§3.6).

### 3.5 API

`crates/fleetdeck-proto/src/board.rs:196-235`, `StationView`:

```rust
    /// Unix ms of the last frame that the deck received from the station
    /// (B22): refreshed every 30 s while a link is up and written when a
    /// link ends. `None`: the station never linked to this deck.
    pub last_seen: Option<i64>,
    /// Unix ms since which the station is reachable without a gap, in this
    /// deck process (the Linked fact, B22). `None` while it is not
    /// reachable. Absent in the answer of an older deck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub linked_at: Option<i64>,
```

- Put `linked_at` directly after `last_seen`. Generated TypeScript (`web/src/gen/board/StationView.ts`, by `make ts-types`): `last_seen: number | null, linked_at?: number`.
- `local::station_view` (`local.rs:116-132`): `linked_at: store.linked_since(&st.station_id),`.
- The member view (`read_routes.rs:20-33`) and the display view (`display_view.rs:146-158`) keep `linked_at` as they keep `last_seen` and `reachable`: it is the same class of fact (when the link of a visible station started), no key, no address.
- No new route, no new event, no new feature flag.

### 3.6 Board

`web/src/board/StationsPanel.tsx:169-173` changes to:

```tsx
          const up = freshness !== 'stale' && station.reachable;
          // `last_seen` is the last time the deck heard the station: the end of its last link
          // (B22). A station that the deck reports online shows no "Last seen": while the board
          // status is stale, "Last reported: online." says what is known (principle 7).
          const lastSeen = station.reachable ? null : station.last_seen;
          // `linked_at` is the start of the current link; an older deck does not send it.
          const linkedAt = up ? (station.linked_at ?? null) : null;
```

- UI text: unchanged. Offline station: `Last seen 8 h ago` (`Last seen now` within the minute), the ISO time as the tooltip. Online station: the Details fact `Linked` with `8 h ago`. Stale status: `Stale link status`, `Last reported: online.` or `Last reported: offline.`; "Last seen" only for a station last reported offline.
- `web/src/board/StationSummary.tsx:1-2`: change the comment to "A station card shows "Last seen" (the end of its last link) only for a station that the deck reports offline (StationsPanel.tsx)."
- No CSS, no new class, no token. The design system needs no change.

### 3.7 Compatibility

1. The board is built into the deck binary (`http/assets.rs:22-23`), so a deck and its board have the same version. Only a board page loaded before a deck upgrade can meet a newer deck: it reads `last_seen` as its Linked time, which then shows "now" or "1 min ago" for an online station until the page reloads. Harmless.
2. A new board with an older deck (the "swap /assets" audit tool only): `linked_at` is absent, so there is no Linked fact. `last_seen` keeps the old meaning there.
3. Stations: no change. The idle time is measured on the deck side; every station version sends the heartbeat.
4. Stored data: no migration. After the upgrade, a station that never links again keeps its old `last_seen` (the start of its last link), the best value known. Every station that links again gets correct values.
5. `fleetdeck deck stations --json` gains `linked_at` for reachable stations (additive).

### 3.8 Errors and edge cases

| Case | Result |
| --- | --- |
| The station closes the link | `last_seen` = the time of its last frame (about the close time). |
| The network dies | The deck ends the link after 45 s; `last_seen` = the time of the last frame, up to 45 s earlier than the end. |
| The deck stops or crashes while links are up | `last_seen` = the last refresh, at most 30 s old. The final write can run on a clean stop, but the deck does not wait for it. |
| Two links of one station overlap (NV-12) | `linked_at` stays at the first `up`; `MAX` keeps the newest `last_seen`. |
| A link ends and the station links again at once | The gap clears `linked_at`; the new link sets a new one. |
| The station is removed while linked | `UPDATE` hits no row: no error. `peers.rs:200` clears the reachability. |
| A database error on a write | A warning `station last_seen`; the link goes on. |
| The deck clock moves back | `MAX` keeps the later value; the board shows "Last seen now" for it until the clock passes it (`relativeAge` clamps a negative age to 0, `web/src/app/sessions.ts:29-31`). Rare; accepted. |
| A station that never linked | `last_seen` `null`, no `linked_at`: no "Last seen", no Linked fact (as now). |

### 3.9 Out of scope

- A station status history (link up and down events). The new log line "station link ended" is the only record.
- "Last seen" in `fleetdeck deck stations` text output, in the MCP `fleet_view`, on the wall or on the phone sidebar.
- A change of the relative-time text ("Last seen now").
- The `last_seen` of wall displays and login sessions (different tables, correct today).

## 4. Risk class

**Normal.** No authentication or authorization change, no deletion, no migration, no repair of stored data (the existing column gets newer values by normal writes), no cost totals, no personal data, no new command path. The API change is one additive optional field and a corrected meaning of `last_seen`; the board is its only reader (§1.3), and it ships in the same binary. One Codex review of the final branch.

## 5. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`.

Cross-track note: the wall-control track (`docs/v2/design/wall-control.md` task B1) also owns `crates/fleetdeck-deck/src/store/mod.rs` and `crates/fleetdeck-proto/src/board.rs`. Do not run T1 in parallel with that task; merge T1 first if possible (it makes `store/mod.rs` about 25 lines shorter).

### T1 (backend): the deck tracks the end and the start of a station link (≈ 330 lines)

- **Owned files:**
  - `crates/fleetdeck-link/src/control.rs`
  - `crates/fleetdeck-link/src/control_tests.rs` (one `#[path]` module line pair at the end)
  - `crates/fleetdeck-link/src/control_rx_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/mod.rs`
  - `crates/fleetdeck-deck/src/store/stations.rs`
  - `crates/fleetdeck-deck/src/store/station_link.rs` (new)
  - `crates/fleetdeck-deck/src/store/station_link_tests.rs` (new)
  - `crates/fleetdeck-deck/src/link/mod.rs`
  - `crates/fleetdeck-deck/src/link/seen.rs` (new)
  - `crates/fleetdeck-deck/src/link/seen_tests.rs` (new)
  - `crates/fleetdeck-deck/src/local.rs`
  - `crates/fleetdeck-deck/src/routing_tests.rs`, `crates/fleetdeck-deck/src/routing_place_tests.rs` (`linked_at: None` in the `StationView` literals)
  - `crates/fleetdeck-deck/tests/station_last_seen.rs` (new)
  - `crates/fleetdeck-proto/src/board.rs`
  - `web/src/gen/board/StationView.ts` (generated only, by `make ts-types`)
- **Changes:** §3.2 (method), §3.3 (module, field type, move, removal of `touch_station`), §3.4 (module and the three `serve` changes), §3.5 (field, doc of `last_seen`, `station_view`). Remove `BTreeSet` from the `store/mod.rs` imports only if nothing else uses it there.
- **Tests:**
  - `control_rx_tests.rs` (declared in `control_tests.rs` like `control_cancel_tests.rs` at `control_tests.rs:697`): `since_last_frame_grows_without_frames_and_resets_on_a_frame`. `with_raw(Role::Deck, feature::ALL, 1 << 20, config)` with `heartbeat` and `idle_limit` of 600 s; after 300 ms without a frame, `since_last_frame() >= 250 ms`; after `send_raw` of a `ControlFrame::Ping`, `since_last_frame()` falls under 100 ms (poll up to 2 s).
  - `station_link_tests.rs`:
    - `station_link_linked_since_keeps_the_first_up_and_clears_on_down`: up at t0 (`Det`), advance 5 s, up again: `linked_since == Some(t0)`; down: `None`, `is_reachable` false; up at t1: `Some(t1)`.
    - `station_link_board_version_grows_only_on_a_change`: two ups give one `watch_board` change; two downs give one.
    - `station_link_heard_never_moves_last_seen_back`: on a paired station with `last_seen` NULL, heard 2000 → 2000; heard 1000 → 2000; heard 3000 → 3000.
    - `station_link_heard_does_not_change_the_board_version`.
    - `station_link_heard_of_an_unknown_station_is_ok`.
    - `station_link_view_has_linked_at_only_while_reachable`: `local::station_view` gives `linked_at == Some(t0)` after up, `None` after down; the JSON of the down view has no `linked_at` key.
  - `seen_tests.rs`:
    - `seen_loop_writes_every_interval_until_the_link_closes`: interval 20 ms, `at` returns 1000, 2000, 3000, …; wait until `last_seen >= 3000`; drop the close sender; the task ends within 10 s; the value does not change after 200 ms more.
    - `seen_loop_writes_nothing_before_the_first_interval`: interval 60 s, close at once: the task ends, `last_seen` stays `None`.
    - `heard_at_ms_subtracts_the_idle_time`: `(10_000, 3 s) → 7_000`; `(1_000, 5 s) → 0`; `(5, Duration::MAX) → 0`.
    - `seen_refresh_is_30_seconds`.
  - `tests/station_last_seen.rs` (with `mod common;`, `fixture()`, `f.connect()`):
    - `the_end_of_a_link_is_the_last_seen_time_and_is_written_before_the_station_goes_offline`: t0 = `f.det.now_ms()`; connect; `linked_since == Some(t0)`; advance the `Det` clock 600 s; subscribe `f.store.watch_board()`; `station.control.close("test", "the station stops")`; on each board change, when `!is_reachable`, read the record: `last_seen >= t0 + 600_000 - 5_000` and `<= f.det.now_ms()`; `linked_since == None`; `station_view(..).linked_at == None`.
- **depends_on:** none.
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-link --no-fail-fast -E 'test(since_last_frame)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(station_link) | test(seen_loop) | test(heard_at) | test(seen_refresh) | binary(station_last_seen) | binary(deck_link) | binary(board_ws) | test(routing) | test(status_projection)'`
  - `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'binary(ts_drift)'`
  - `cargo clippy -p fleetdeck-link -p fleetdeck-deck -p fleetdeck-proto --all-targets -- -D warnings`
  - The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - `grep -rn touch_station crates/` prints nothing.

### T2 (ux): the board reads `linked_at` and shows "Last seen" only for an offline station (≈ 70 lines)

- **Owned files:** `web/src/board/StationsPanel.tsx`, `web/src/board/StationSummary.tsx`, `web/src/board/StationsPanel.test.tsx`, `web/src/board/fleet.test.tsx`.
- **Changes:** §3.6. In the tests, an online station gets its Linked time from `linked_at`, not from `last_seen`.
  - `StationsPanel.test.tsx:17-22`: give `linked_at: Date.now() - HOURS_8, last_seen: Date.now()`; expect `<dt>Linked</dt>` and `8 h ago`, no "Last seen". Update the comment at line 17 to the new meaning.
  - `fleet.test.tsx:149-187`: station-a gets `linked_at: 0` in place of `last_seen: 0` (the ISO `1970-01-01T00:00:00.000Z` is the Linked tooltip); update the comment at lines 180-182.
- **Tests to add** (`StationsPanel.test.tsx`, `describe('the station header meta')`):
  - `an online station from an older deck (no linked_at) shows no Linked fact and no "Last seen"`.
  - `an offline station shows "Last seen" from last_seen, not from linked_at`: `reachable: false, last_seen: now - 5 min, linked_at: now - 8 h` → `Last seen 5 min ago`, no Linked.
  - `a stale status shows no "Last seen" for a station last reported online`: `freshness="stale"`, `reachable: true, last_seen: now - 8 h` → `Last reported: online.`, no "Last seen".
  - `a stale status keeps "Last seen" for a station last reported offline`: `reachable: false, last_seen: now - 8 h`, stale → `Last seen 8 h ago` and `Last reported: offline.`.
- **depends_on:** T1 (the generated `StationView` type).
- **Acceptance:**
  - `cd web && bun run typecheck && bun run lint`
  - `cd web && bun run test src/board` (through heavy.sh)
  - `sh tools/design-check.sh`
  - The guards (as T1).
  - Gallery shot `09 stations` in both themes, desktop and phone (the fleet-design-system skill §3 step 3): no visual change expected.

## 6. Decisions made in this draft

1. **`last_seen` keeps its name and gets the meaning "last frame heard".** Rejected: a new column `last_heard`/`link_ended_at` beside `last_seen` = link start. That needs a migration (high risk class) and keeps a misleading field name that caused this bug.
2. **The link start lives in memory (`linked_at`), not in a column.** Every link that is up belongs to the running deck process. Rejected: a stored `linked_at` column; no reader needs a stored start, and there is no status history.
3. **Refresh every 30 s while the link lives, plus a write at the end.** Rejected: a write only at the end (lost on a deck stop or crash, §2 Q2); a write on every heartbeat (a database write every 15 s per station for no visible gain).
4. **The end time is the last frame, not the time of the close.** Needs the read-only `Control::since_last_frame()`. Rejected: the close time (up to 45 s late after a network loss).
5. **`MAX` in the update: the time never moves back.** Rejected: a plain assignment (an overlapping old link can write an older time after a new link). Accepted cost: a backward clock jump keeps a future value for a while (§3.8).
6. **The refresh writes do not call `board_changed()`.** Rejected: a board delta to every socket every 30 s for each station. The end write needs none either: the reachability change after it sends the delta.
7. **The end write happens in `serve`, before `LiveGuard` drops.** Rejected: an async write from `LiveGuard::drop` (a `Drop` cannot await; a spawned write races with the board delta).
8. **`linked_at` is the start of the continuous reachability** (the first `up` after a gap), not the start of the newest link. Rejected: the newest link (a NV-12 replacement would reset "Linked" with no visible gap).
9. **The board shows "Last seen" only when the deck reports the station offline**, also when the board status is stale. Rejected: keep the stale case (it shows a value that can be hours old for a station last reported online).
10. **`linked_at` is optional on the wire** (`skip_serializing_if`, `ts(optional)`), like `coordinator_live`. Rejected: a required field (it would break the test helpers and older JSON readers for no gain).
11. **Members and wall displays get `linked_at`** like `last_seen`. Rejected: strip it (it shows no key and no address; `last_seen` is already sent).
12. **New log line "station link ended" with the close reason.** Rejected: no log (the journal could not show the end of a link during this research).
13. **Reachability code moves to `store/station_link.rs`.** `store/mod.rs` is at 796 lines (limit 800). Rejected: grow `store/mod.rs`.
14. **No feature flag.** Board and deck ship in one binary; the station is not involved. Rejected: a link or board feature flag.

## 7. Owner questions

1. **Should "Last seen" within the first minute read "Last seen just now" in place of "Last seen now"?** It is the shared `relativeAge` text of the board (also the Linked fact). Recommended default: keep "now" (no change in this track; a copy pass can change it for all places at once).
2. **Is a 30 s refresh good enough after a deck crash or restart?** A shorter interval writes more often. Recommended default: 30 s.

## 8. Recordings and sessions

- `evidence-live-deck.txt` in `/home/luismorales/fleetdeck-briefs/backlog/last-seen/`: the station rows of the live `deck.db` (opened read only) and the "station link up" lines of the deck journal. No provider CLI was run and no Fleet Deck session was started: the bug is in the deck and the facts were in the code, the database and the journal.

## Decision log entries (to append)

## DL-490: B22 `stations.last_seen` is the last frame heard, refreshed while the link lives (design agent, 2026-10-23)

- Context: B22; design `docs/v2/design/last-seen.md` §1-§3. The deck wrote `last_seen` only at link start (`link/mod.rs:207`), so an offline station showed the start of its last link as "Last seen".
- Decision: `last_seen` is the deck time of the last frame from the station (`now - Control::since_last_frame()`). The deck writes it at link start, every 30 s while the link lives, and in `serve` when a link ends, before the station is marked unreachable. The SQL uses `MAX`: the time never moves back. The writes do not bump the board version. No schema change.
- Rejected: a new `link_ended_at` column with a migration; a write only at link close (lost on a deck stop); the close time (up to 45 s late).

## DL-491: B22 `StationView.linked_at` is the start of the continuous link, in memory (design agent, 2026-10-23)

- Decision: the deck keeps, beside the reachability (`BTreeMap<StationId, i64>`), the time of the first `up` after a gap and sends it as the optional `StationView.linked_at`. The board's Linked fact reads it. No column: a live link always belongs to the running deck process.
- Rejected: a stored link start (no reader, no status history).

## DL-492: B22 the board shows "Last seen" only for a station that the deck reports offline (design agent, 2026-10-23)

- Decision: `StationsPanel` shows "Last seen" when `station.reachable` is false, in live and stale status; a station last reported online shows "Last reported: online." and no "Last seen" while the status is stale.
- Rejected: the stale case of today, which showed the link start of a station last reported online.
