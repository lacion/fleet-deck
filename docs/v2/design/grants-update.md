# A station update keeps the fleet grants of its sessions (B26)

- **Status:** Draft for the architect, revision 1 (2026-10-24). Risk: **high** (an authorization change: how long a grant stays in effect, and three new ways that a grant ends; §5). The adversary does a DESIGN REVIEW before the build starts and a PRE-PR REVIEW before the merge.
- **Backlog:** `docs/v2/BACKLOG.md` item B26 (lines 330-341).
- **Code base:** branch `v2`, HEAD `d1fa4205`. All file:line references are on this commit.
- **Evidence:** `/home/luismorales/fleetdeck-briefs/backlog2/grants-update/evidence-live-deck.txt` (the owner's live deck database and the Lighting station database, both opened with `sqlite3 -readonly`; the `fleetdeck-up` journal of Lighting). No session was started, and nothing was written.

## 1. Verified facts

### 1.1 What the live data shows (Lighting, 2026-10-24, read only)

1. **No station key changed and no station incarnation changed.** Lighting was paired on 2026-10-12, dev1 and mac-m4 on 2026-10-15, thunder on 2026-10-22 (its first pairing, audit row 1018). Each `stations.fingerprint` is the pairing fingerprint, and the Lighting station `meta.incarnation_id` is `01M4RP85JT…` since its database was made. Lighting restarted 14 times from 2026-10-20 to 2026-10-24 (journal).
2. **No grant ended for an update reason.** The 13 `tokens` rows end as: `expired`/`idle` and `expired`/`expired` (coordinator, 4), `revoked`/`owner` (3), worker `revoked`/`session_ended` (3), worker `revoked`/`expired` (2), worker `revoked`/`start_failed` (1). There is no `station_*`, `identity_changed` or `rotated` reason.
3. **The owner's report, 2026-10-23, is an idle expiry.** The architect session `01M5MQNPSE` on Lighting had grant `01M5MQNPRZ` (created 15:57:01 UTC). Its last fleet call was the `coordinator.stop` at 16:36:07 (audit row 1282). The deck expired it for idle time at 17:07:59 (audit row 1284), while run 10 of the session was running (17:05:03-17:12:00, station events 211468-211806) and the owner updated the stations. Lighting restarted at 17:24:52 UTC. At 17:27:14 the owner wrote "ok all stations updated"; at 17:27:47 the session answered "The update broke my coordinator tools … `coordinator.query needs scope coordinate, which the pairing did not grant`" and, at 17:29:30, "the update expired my fleet grant … My last call was at 16:37 UTC. After that I only waited for the workflow". The owner renewed (17:33:54), then revoked and gave a new grant (17:35:16, 17:35:38).
4. **The report of 2026-10-22 has the same shape.** The dev1 coordinator grant `01M5HKT9EG` was used at 10:55:07 and expired for idle time at 11:25:33 (audit rows 966, 971). The session process ended at 12:45:53 (`session.ended process_exit`), the owner resumed it at 15:26:57, and the station bound the grant again at generation 2 at 15:27:17 (deck timeline seq 32211). The deck kept the grant `expired`: a higher generation never revives an ended grant (§1.3 item 3). The deck timeline of that session keeps only the rows after 11:45 (the 500-row cap), so it does not show whether the session worked between 10:55 and 11:25.
5. **The station start ends the bindings of grants past their lifetime.** At the restarts of 2026-10-23 11:37:14 and 2026-10-24 16:22:58 the station appended `coordinator.ended { reason: "expired" }` for grants whose lifetime had passed (station events 207587, 207588, 225722). This is correct (§1.2 item 5).
6. **Lifecycle ends have no audit row.** The worker grant `01M5J75CH6` (thunder) is `revoked`/`session_ended`, but the audit log has only its `coordinator.worker_grant_created` row (1066). The only deck-side end with a row is the sweep's `coordinator.grant_expired`.

### 1.2 The station side of a restart

1. The station incarnation changes only with a new database: `Store::open` reads `meta.incarnation_id` and writes a new one only when the `meta` row is missing (`crates/fleetdeck-station/src/store/mod.rs:18-19`, `:395-421`). A rollback that restores the pre-upgrade snapshot gets a new incarnation (`crates/fleetdeck-station/src/upgrade.rs:17-20`; `docs/v2/operations.md:1386-1388`).
2. The host decides whether a session outlives a station restart: `host = shim` (the Linux default) keeps the agents running through a restart or an upgrade; `host = direct` (the default elsewhere, so mac-m4) stops them with the station (`docs/v2/operations.md:36`, `:1390`). The Lighting journal shows the `claude` and `codex` processes "remain running after unit stopped".
3. **Shim host.** At the start, the station loads the open `shim_bindings` rows and holds them until their shim replayed its output (`crates/fleetdeck-station/src/manager/binding_start.rs:1-14`, `load` at `:65-140`). A row of a grant past its lifetime ends with `expired` (`:121-129`, `Store::shim_binding_expire` at `crates/fleetdeck-station/src/store/shim_bindings_start.rs:188-202`). The rows of shims that were not reattached are deleted with `coordinator.ended { reason: "process_exit" }` (`shim_bindings_drop_others`, `shim_bindings_start.rs:238-260`). A row that the fence names, or every row when the fence is unreadable, is suspended (`binding_start.rs:97-117`); no event goes to the deck then. On Lighting the fence file `~/.local/share/fleetdeck/station/run/bindings.fence` has only its header.
4. **Direct host.** The bindings are in memory only. After a restart the recovery marks each session `lost` with the detail "station restart: the provider process is gone" (`crates/fleetdeck-station/src/store/recovery.rs:170-188`); it appends no `session.ended` and no `coordinator.ended`. So the deck still has the grants `active`, and their idle clock runs.
5. **A grant with a station record (MCP P4b) belongs to the session, not to the process.** A process end of such a binding writes no event (`ended_events`, `crates/fleetdeck-station/src/manager/coordinator_start.rs:114-135`; `evented`, `crates/fleetdeck-station/src/store/shim_bindings.rs:292-314`; `ended_event`, `shim_bindings_start.rs:125-157`). The next start or resume of the session gives the grant again at a higher generation and checks the native identity (`RecordStart`, `coordinator_start.rs:40-47`; `tools_generic`, `crates/fleetdeck-station/src/manager/coordinator_tools.rs:185-230`). Only a coordinator grant gets a record: "an owner grant (`kind` coordinator, never a worker)" (`record_start`, `crates/fleetdeck-station/src/manager/coordinator_set.rs:253-288`).
6. **A worker grant (MCP P4a) belongs to its process.** "A resumed start never gets a grant" (`RESUMED`, `crates/fleetdeck-station/src/manager/worker_grant.rs:31-32`; `grant_tools` returns it for every resume, `:207-210`). The skip appends `coordinator.worker_outcome { outcome: skipped, reason: "resumed" }` (`WorkerSkip::event`, `:57-76`). P4a left a grant on resume out of scope (W2, `docs/v2/design/mcp-p4a.md:5`, `:45`).

### 1.3 The deck side

1. A grant is in effect when it is `active`, not past `expires_at`, and not idle (`in_effect`, `crates/fleetdeck-deck/src/store/grant_scopes.rs:42-52`; `Grant::idle_since`, `crates/fleetdeck-deck/src/store/grants.rs:254-259`). The sweep (`sweep_grants`, `grant_scopes.rs:213-248`) first records running turns as use (G6, `turns::touch_running_turns`), then ends stale grants (`end_stale`, `:107-131`) and writes one `coordinator.grant_expired` row per end. It runs at the deck start, before any link (`crates/fleetdeck-deck/src/store/mod.rs:435-441`), and every 30 s (`GRANT_SWEEP_EVERY`, `crates/fleetdeck-deck/src/serve.rs:59`).
2. G6 touches a bound grant only while its station has a live link that answered `station.info` and a read model at the head of that answer (`crates/fleetdeck-deck/src/store/grant_turns.rs:1-8`, `touch_if_running` at `:53-78`). While a station is offline, nothing touches the grants of its sessions, so a gap longer than the rest of the idle time ends them.
3. The lifecycle rules (`record_lifecycle`, `crates/fleetdeck-deck/src/store/grants_lifecycle.rs:36-114`): `coordinator.bound` binds an active grant once or moves it to a higher generation; it "never revives an expired or revoked grant (a renewal needs the owner's renew command)" (`:39-42`, `apply_bound` at `:130-223`; the rebind of a grant with a row needs `in_effect`, `:168-175`). `coordinator.ended` sets the grant `expired` for `idle` and `revoked` for any other reason (`apply_ended`, `:250-284`). `session.ended` revokes the worker grants of the session with `session_ended` (`apply_session_ended`, `crates/fleetdeck-deck/src/store/worker_grant_end.rs:15-42`). A worker outcome revokes with `handoff_skipped` (`apply_outcome`, `crates/fleetdeck-deck/src/store/worker_grant.rs:742-767`). None of them writes an audit row.
4. The deck has no rule for a station update. It keeps `station.info` (with `version` and `incarnation`) in memory only (`StationInfo`, `crates/fleetdeck-proto/src/link.rs:244-253`; `record_station_info`, `crates/fleetdeck-deck/src/store/station_machines.rs:16-22`). The read model stores `stations.incarnation_id` and `cursor` (`crates/fleetdeck-deck/src/store/schema.rs:31-45`).
5. `stations.last_seen` is the deck time of the last frame of the station. The deck writes it at the link start, every 30 s while the link lives, and at its end, before the station becomes unreachable (`crates/fleetdeck-deck/src/link/seen.rs:1-69`; `serve`, `crates/fleetdeck-deck/src/link/mod.rs:185-279`; `LiveGuard::drop`, `:171-182`). Reachability is memory only (`station_link.rs:78-107`); at the deck start no station is reachable.
6. A key rotation keeps the station row: "the id, name, grants, address and cursor stay" (`rotate_station`, `crates/fleetdeck-deck/src/store/stations.rs:287-313`). A new key is a new pairing: `station_id` is the fingerprint at pairing (`schema.rs:29-32`). The removal of a station deletes its row (`revoke_station`, `stations.rs:240-284`); its doc says "its pin, grants, sessions and timeline", but no statement touches `tokens`, so the grants of its sessions stay `active` until they idle out.
7. The board shows an idle-expired grant with Renew (`crates/fleetdeck-deck/src/http/grant_routes.rs:186-230`), and hides revoked grants from the access column (`accessRows`, `web/src/fleet/FleetAccess.tsx:49-60`; `grantState` at `:34-43`).
8. The deck settings are fields of `deck.toml` (`DeckConfig`, `crates/fleetdeck-core/src/config_deck.rs:40-80`; the board fields, `crates/fleetdeck-core/src/settings/fields.rs:354-539`; the elevation rule, `DeckHolder::danger`, `crates/fleetdeck-core/src/settings/holder.rs:369-410`). The store keeps a copy of the owner knobs (`DeckKnobs`, `crates/fleetdeck-deck/src/knobs.rs:19-97`; `set_knobs` at `serve.rs:230-231` and in `publish`, `crates/fleetdeck-deck/src/http/settings_routes.rs:517-522`).

### 1.4 Corrections to the backlog

1. The backlog is right that no code clears the grants on an update. A version update keeps the station key and the incarnation (§1.1 item 1, §1.2 item 1). The grants are lost by the **idle clock**: it runs while the station is offline and while the session waits (before G6), and a later bind never revives an ended grant (§1.3 items 2 and 3).
2. G6 (merged 2026-10-24, `c99585f9`) already keeps a grant in use while its session works, also with background work only (`turn_running`, `grant_turns.rs:23-32`). That covers the case of §1.1 item 3. It does not cover the time while the station of the session is offline (§1.3 item 2).
3. The column is `idle_ttl_ms`, not `idle_ttl_m`.

## 2. Answers to the questions of the backlog

1. **Is the drop intended for a key change and not for a plain version update?** There is no drop rule today. A key change makes a new station row, so no old grant applies to it; this stays. A version update keeps the station row, the key and the incarnation; with this design the grants of its sessions stay in use across the offline gap (§3.2) and are kept when the station comes back (§3.3).
2. **Make it a setting.** `keep_grants_on_update` in `deck.toml`, on by default, a danger setting (turning it on needs the elevation). Off: a station that comes back with a new version ends the grants of its sessions (`station_updated`). The board shows a warning while it is off (§3.6).
3. **The audit log records every grant that an update keeps or drops.** New actions `coordinator.grant_kept` and `coordinator.grant_dropped` (§3.5). A drop by a station event (process end, session end, worker outcome, snapshot) gets a row too.
4. **Which cases did the research distinguish?** (a) the idle expiry during the update: the main cause, fixed by the hold (§3.2); (b) the incarnation change: it does not happen on an update; a real reset now drops the grants on purpose (§3.3); (c) worker grants of a resumed session: not given again, by the P4a rule; out of scope (owner question Q3); (d) a key change: a new station; the removal of the old row now revokes its grants (§3.4).

## 3. Design

### 3.1 Semantics

1. **Hold.** While the station of a grant's session has no live link, the deck counts the time as use of the grant, for at most 60 minutes after the last frame of that station (`GRANT_HOLD_MAX_MS`). Only a bound grant that was in effect at that last frame is held. The lifetime (`expires_at`) stays the hard cap. Nothing revives a grant that ended.
2. **Return.** At each `station.info` answer of a live link, the deck compares the incarnation and the version with the last answer it stored:
   - another incarnation (a new or restored station database): the deck ends every `active` grant of the station's sessions (`revoked`, `station_reset`), with either setting;
   - another version and `keep_grants_on_update = false`: the deck ends them (`revoked`, `station_updated`);
   - another version and the setting on: the grants stay; one `coordinator.grant_kept` row per grant;
   - the same incarnation and version: the held grants stay; one `coordinator.grant_kept` row per held grant.
3. **Key change.** A new key is a new station. When the owner removes the old station, the deck revokes the grants of its sessions (`station_removed`). A signed key rotation keeps the row and its grants (one `coordinator.grant_kept` row per active grant, reason `key_rotated`).
4. "The grants of a station" always means the grants whose `tokens.station_id` is that station (the sessions that run there). A grant of another station that only names it in `stations` is not changed.

### 3.2 The hold (deck store, `store/grant_hold.rs`, new)

The module is a child of `grant_scopes.rs` (like `turns`): `#[path = "grant_hold.rs"] pub(crate) mod hold;`.

```rust
//! B26: the grants of a station's sessions survive its update or restart
//! (design `grants-update.md` §3.2-§3.3).

/// How long after the last frame of an offline station the deck counts the
/// time as use of the bound grants of its sessions.
pub const GRANT_HOLD_MAX_MS: i64 = 60 * 60 * 1000;

/// The sweep step (touch point 5): every `active`, bound grant whose station
/// has no live link, whose station was last heard at most
/// `GRANT_HOLD_MAX_MS` ago (and not after `now`), and that was in effect at
/// that last frame, gets `last_used_at = now` and a `grant_holds` row (kept
/// when it exists). Then the holds of grants that are not `active` go.
/// Returns the number of held grants.
pub(in crate::store) fn hold_offline(tx: &Connection, inner: &Inner, now: i64) -> Result<usize> {
    let online: BTreeSet<StationId> = inner
        .reachable
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .cloned()
        .collect();
    let mut gaps: BTreeMap<StationId, i64> = BTreeMap::new();
    {
        let mut stmt =
            tx.prepare("SELECT station_id, last_seen FROM stations WHERE last_seen IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        for row in rows {
            let (id, seen) = row?;
            let id = StationId::from(id);
            if !online.contains(&id) && seen <= now && now - seen <= GRANT_HOLD_MAX_MS {
                gaps.insert(id, seen);
            }
        }
    }
    let mut held = 0;
    for grant in active_grants(tx)? {
        let Some(&since) = gaps.get(&grant.station_id) else { continue };
        if grant.binding.is_none() || now > grant.expires_at || !in_effect(&grant, since) {
            continue;
        }
        tx.execute(
            "INSERT INTO grant_holds (grant_id, station_id, since) VALUES (?1, ?2, ?3)
             ON CONFLICT (grant_id) DO NOTHING",
            params![grant.grant_id, grant.station_id.as_str(), since],
        )?;
        held += tx.execute(
            "UPDATE tokens SET last_used_at = MAX(COALESCE(last_used_at, ?2), ?2)
             WHERE grant_id = ?1 AND state = 'active'",
            params![grant.grant_id, now],
        )?;
    }
    tx.execute(
        "DELETE FROM grant_holds
         WHERE grant_id NOT IN (SELECT grant_id FROM tokens WHERE state = 'active')",
        [],
    )?;
    Ok(held)
}
```

1. `sweep_grants` calls it after `turns::touch_running_turns` and before `end_stale`, in the same transaction and with the same `now` (`grant_scopes.rs:226-228`). It logs `tracing::debug!(held, "offline stations kept their grants in use")` when `held > 0`.
2. The hold does not read the setting. At the deck start (`DeckStore::open` sweeps before `serve` sets the knobs), every station is offline, and the durable `last_seen` gives each one its gap. A deck that was down for more than 60 minutes holds nothing.
3. The reachability lock is taken and released before the first statement; no lock is held across a database call.
4. When a station stays offline for more than 60 minutes, the touches stop. The grant then ends by the idle rule one idle time after the last touch, with today's `coordinator.grant_expired` row.

### 3.3 The return (deck store, `store/grant_hold.rs`)

```rust
/// What one `station.info` answer settled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub kept: usize,
    pub dropped: usize,
}

/// §3.3 in the caller's transaction. `answer` is the incarnation and the
/// version of the answer; `keep` is `keep_grants_on_update`.
pub(in crate::store) fn resolve_tx(
    tx: &Connection,
    ids: &dyn IdSource,
    station: &StationId,
    (incarnation, version): (&IncarnationId, Option<&str>),
    (keep, delivery_ms): (bool, i64),
    now: i64,
) -> Result<(Resolved, BTreeSet<StationId>)>;

impl DeckStore {
    /// Settles the holds and the update rule of `station` with one
    /// `station.info` answer of live link `token`. Nothing is written when
    /// the link ended. Notes the changed scopes; wakes the command router
    /// when it dropped a grant.
    ///
    /// # Errors
    /// A database error.
    pub async fn resolve_grant_holds(
        &self,
        station: StationId,
        token: u64,
        incarnation: IncarnationId,
        version: Option<String>,
    ) -> Result<Resolved>;
}
```

`resolve_tx`, in this order:

1. Read `stations.info_incarnation` and `stations.info_version` (V51, §3.7). No row: return `Resolved::default()`.
2. Write `info_incarnation = incarnation` and `info_version = COALESCE(version, info_version)`.
3. `reset = old_incarnation.is_some_and(|o| o != incarnation)`; `updated = matches!((old_version, version), (Some(a), Some(b)) if a != b)`.
4. Read and then delete every `grant_holds` row of the station (`grant_id → since`).
5. For each `active` grant of the station (`grant_audit::active_of_station`, §3.5), in `grant_id` order:
   - `reset`: end it with `station_reset` (item 6); one `coordinator.grant_dropped` row `reason=station_reset;station=<name>`;
   - else `updated && !keep`: end it with `station_updated`; one row `coordinator.grant_dropped` `reason=station_updated;station=<name>;from=<a>;to=<b>`;
   - else `updated`: one row `coordinator.grant_kept` `reason=station_updated;station=<name>;from=<a>;to=<b>`, plus `;offline_ms=<now - since>` when the grant had a hold;
   - else, when the grant had a hold: one row `coordinator.grant_kept` `reason=station_offline;station=<name>;offline_ms=<now - since>`;
   - else nothing.
   Each kept row adds 1 to `kept`; each end adds 1 to `dropped`.
6. **The end of a grant** (`end_grant_tx`, private): when the grant has a `session_owner_grant` row, `owner_grants::revoke_with_row_tx(tx, ids, grant_id, reason, now)` (it queues `session.coordinator_set` with no spec, `owner_grants.rs:467-492`). Else `worker_grant::revoke_tx(tx, grant_id, reason, now)`, and, when that changed the row and `session_id` is not empty, `grants_lifecycle::queue_revoke(tx, station, session, grant_id, (ids, now, delivery_ms))` (the `session.coordinator_revoke` of the outbox, `grants_lifecycle.rs:225-248`). The deck refuses at once; the station learns it like an owner revoke.

`DeckStore::resolve_grant_holds` runs in one `call` closure: it first checks that `inner.link_features` holds `token` for `station` (the pattern of `Inner::set_link_stream`, `station_link.rs:17-40`; else it returns `Resolved::default()` and writes nothing), then reads `inner.knobs()` (`keep_grants_on_update`, `delivery_ms`), opens a transaction, reads the clock (`now_ms(&inner.clock)`), calls `resolve_tx`, and commits. After the call: `note_scope_changes(changed)`, and `commands_changed()` when `dropped > 0`.

**The call site** is the `ask` closure of `fetch_info` (`crates/fleetdeck-deck/src/link/info.rs:47-67`), right after `record_link_stream`:

```rust
if let Ok(Some(info)) = &answer
    && let Err(e) = ctx
        .store
        .resolve_grant_holds(id.clone(), token, info.incarnation.clone(), info.version.clone())
        .await
{
    tracing::warn!(error = %e, "grant holds");
}
```

The first answer comes at once after the link is up, then every 60 s (`INFO_REFRESH`, `info.rs:17`). An update is seen once: after the first answer the stored version is the new one.

### 3.4 Station removal and key rotation (`store/stations.rs`)

1. `revoke_station` (`stations.rs:247-284`): inside `if let Some(rec) = &found`, before the `DELETE FROM stations`, call `super::grant_audit::drop_station_tx(&tx, &rec.station_id, &rec.name, now)?`. It revokes every grant of the station that is not `revoked` (`active` or `expired`) with `station_removed` through `worker_grant::revoke_tx`, and writes one `coordinator.grant_dropped` row `reason=station_removed;station=<name>` for each grant that `revoke_tx` changed. It sends nothing to the station (its link closes). The closure returns `(found, changed)`; after the commit, `revoke_station` calls `self.note_scope_changes(changed)` (a removed station is a no-op for `DeckPeers::refresh_one`, `peers.rs:131-142`). The `grant_holds` rows go by `ON DELETE CASCADE`.
2. `rotate_station` (`stations.rs:294-314`): after the `UPDATE`, call `super::grant_audit::kept_on_rotation_tx(&tx, &new_key_hex, now)?`: it finds the station by its new key and writes one `coordinator.grant_kept` row `reason=key_rotated;station=<name>` for each `active` grant of it. `now` is `self.now_ms()`, read before the call.

### 3.5 Audit rows (`store/grant_audit.rs`, new)

```rust
//! The audit rows of grants that a station update, restart, reset, removal
//! or event keeps or drops (B26, design `grants-update.md` §3.5).

pub(crate) const KEPT: &str = "coordinator.grant_kept";
pub(crate) const DROPPED: &str = "coordinator.grant_dropped";
pub(crate) const REASON_STATION_UPDATED: &str = "station_updated";
pub(crate) const REASON_STATION_RESET: &str = "station_reset";
pub(crate) const REASON_STATION_REMOVED: &str = "station_removed";
pub(crate) const REASON_STATION_OFFLINE: &str = "station_offline";
pub(crate) const REASON_KEY_ROTATED: &str = "key_rotated";

/// One audit row (actor `deck`, result `ok`, `command_id` NULL) in the caller's transaction.
pub(crate) fn audit_tx(tx: &Connection, now: i64, action: &str, grant_id: &str, detail: &str) -> Result<()>;
/// The name of `station`, or `?` when it has no row.
pub(crate) fn station_name(tx: &Connection, station: &StationId) -> Result<String>;
/// The ids of the `active` grants whose `station_id` is `station`.
pub(crate) fn active_of_station(tx: &Connection, station: &StationId) -> Result<BTreeSet<String>>;
/// One `coordinator.grant_dropped` row for each id of `before` that is no longer
/// `active`: `reason=<revoked_reason or state>;cause=<cause>;station=<name>`. Returns the count.
pub(crate) fn audit_ends_tx(tx: &Connection, station: &StationId, before: &BTreeSet<String>, cause: &str, now: i64) -> Result<usize>;
/// §3.4 item 1.
pub(crate) fn drop_station_tx(tx: &Connection, station: &StationId, name: &str, now: i64) -> Result<BTreeSet<StationId>>;
/// §3.4 item 2.
pub(crate) fn kept_on_rotation_tx(tx: &Connection, new_key_hex: &str, now: i64) -> Result<usize>;
```

1. The detail is `key=value` pairs joined by `;` (the form of `coordinator.grant_created`). It holds only the reason, the cause, the station name, the versions and `offline_ms`. Never a credential, a digest or a session text.
2. **Lifecycle ends.** `record_lifecycle` (`grants_lifecycle.rs:57-114`) gets a wrapper: the old body becomes `fn apply_lifecycle(..)` (same signature). The new `record_lifecycle` returns early for every other event, as today. For the four kinds it reads `before = active_of_station(tx, station)`, calls `apply_lifecycle`, and then `audit_ends_tx(tx, station, &before, cause, now)`. The cause:

   | Event | `cause` |
   | --- | --- |
   | `coordinator.bound` (the worker cap ends a grant) | `bound` |
   | `coordinator.ended { reason }` | `reason` (for example `process_exit`, `identity_changed`, `expired`, `rotated`) |
   | `coordinator.worker_outcome { reason }` | `reason` (for example `resumed`) |
   | `session.ended` | `session_ended` |

   `apply_projection` (`grants_lifecycle.rs:400-504`) gets the same wrapper with cause `snapshot`. A duplicate or an event that changes no row writes no row. An owner revoke that the station reports back changes nothing (the row is `revoked` already), so it writes no second row.
3. `queue_revoke` (`grants_lifecycle.rs:227`) becomes `pub(super)`, for §3.3 item 6.
4. The module is declared in `store/mod.rs` next to `mod grants_lifecycle;` (`mod.rs:71`): `mod grant_audit;`.

### 3.6 Setting, texts and board

1. **`deck.toml`:** top-level `keep_grants_on_update = true` (the default). `DeckConfig.keep_grants_on_update: bool` with the doc "`keep_grants_on_update` (B26): a station that comes back with a new version keeps the fleet grants of its sessions. On by default."; `RAW_DECK_KEYS` and `RawDeck` get the key; `parse` uses `raw.keep_grants_on_update.unwrap_or(true)`.
2. **The field** (`deck_fields`, right after the `fleet_control` entry, `fields.rs:387-394`):

   ```rust
   field(
       "keep_grants_on_update",
       "Keep grants on update",
       K::Bool,
       Live,
       "When a station comes back after an update, its sessions keep their fleet grants. Off: a station that comes back with a new version ends the grants of its sessions, and you give them again. A station with new data, or a station that you remove, always ends them.",
   )
   .danger(),
   ```

   `DeckHolder` writes the value (`m.insert("keep_grants_on_update".into(), json!(c.keep_grants_on_update));` after `fleet_control`, `holder.rs:351`), and `danger` adds the key when `!old.keep_grants_on_update && new.keep_grants_on_update` (after the `fleet_control` rule, `holder.rs:378-381`).
3. **The store copy:** `DeckKnobs.keep_grants_on_update: bool` (doc "A station that comes back with a new version keeps the grants of its sessions (B26)."), `true` in `Default`, `config.keep_grants_on_update` in `of`. `set_knobs` already runs at the start and at each save, so no other wiring changes.
4. **The refusal texts** (`store/grant_end.rs`): two new variants `GrantEnd::StationUpdated` and `GrantEnd::StationReset`. `grant_end` maps `GrantState::Revoked` with `revoked_reason` `station_updated` or `station_reset` to them (else `Revoked`). `EndFacts::at` keeps them like `Revoked`. `end_text`:
   - `StationUpdated`: "Your fleet grant ended when your station was updated, because the deck setting \"Keep grants on update\" is off. The owner can give a new grant on the board."
   - `StationReset`: "Your fleet grant ended because your station started with new data. The owner can give a new grant on the board."
5. **Board:**
   - `web/src/settings/model.ts:51`: the "Fleet control" section lists `['fleet_control', 'allow_fleet_control', 'keep_grants_on_update']`.
   - `web/src/settings/SettingsFields.tsx` `hazardOf` (`:171-180`): also returns a hazard for `meta.key === 'keep_grants_on_update' && value === false`, with the text "Each station update ends the fleet grants of the sessions on that station. You give them again after the update."
   - `web/src/fleet/FleetAccess.tsx` `grantState` (`:38-43`): a `revoked` grant says `ended by a station update` (`station_updated`), `ended: the station has new data` (`station_reset`), `ended: the station was removed` (`station_removed`); other revoked grants stay `revoked`.

### 3.7 Migration (deck V51, `store/grant_hold_schema.rs`, new)

```rust
//! V51 (B26): the last `station.info` incarnation and version of each
//! station, and the grants that the deck holds while their station is
//! offline. To reverse it, drop the table and the two columns.

pub(super) const SQL: &str = "
ALTER TABLE stations ADD COLUMN info_incarnation TEXT;
ALTER TABLE stations ADD COLUMN info_version TEXT;
CREATE TABLE grant_holds (
    grant_id   TEXT    PRIMARY KEY,
    station_id TEXT    NOT NULL REFERENCES stations(station_id) ON DELETE CASCADE,
    since      INTEGER NOT NULL
) STRICT;
CREATE INDEX grant_holds_by_station ON grant_holds (station_id);
";
```

`schema.rs` (799 lines): the comment at `:790` becomes "V48 to V51: wall pages and the page mode (B17), the wake key with the session (G5), displays.allow_apps (B17 wall apps), the grant holds (B26)." and `M::up(super::grant_hold_schema::SQL),` follows `:793`. To stay under 800 lines, join the two comment lines of V39 (`:774-775`) into one: `// V39 (BG-3): setup reports of unpaired keys and their retry permissions (provision_reports.rs).` The module is declared in `grant_scopes.rs` as `#[path = "grant_hold_schema.rs"] pub(super) mod hold_schema;`, so the list reads `super::grant_scopes::hold_schema::SQL`. The merge renumbers when another branch takes V51.

### 3.8 Compatibility

1. **Stations:** no station change, no protocol change, no link feature. Every station version works. A station built before `StationInfo.version` (M2 P4-2) never counts as updated, so the off rule never drops its grants; the hold and the reset rule still work.
2. **Older deck:** V51 adds two nullable columns and a table. A downgrade needs the pre-upgrade backup, as for every migration (`docs/v2/operations.md` §13.6). After the upgrade the first answer of each station only stores the incarnation and the version: nothing is dropped or kept on the first answer.
3. **The setting:** absent from an older `deck.toml` → on. The board takes the new field from the deck's field list; nothing in `web/src/gen` changes.
4. **G6:** unchanged. The hold is a fifth touch point with its own rule, only while the station has no live link; G6 touches only while it has one.
5. **P4b and P4a:** unchanged. A grant with a record comes back at the next process start (§1.2 item 5) while it is in effect; the hold keeps it in effect across the gap. A worker grant still ends when its process ends; a resumed worker still gets no grant (Q3).
6. **ADR-006 §5.3** gets one sentence after the G6 sentence (`docs/v2/adr/ADR-006-coordinator-mcp.md:84`): "While the station of the grant's session has no live link, the deck also refreshes `last_used_at` of a bound grant that was in effect at the last frame of that station, for at most 60 minutes after that frame (B26, DL-531)." ADR-008 does not change.

### 3.9 Errors and edge cases

| Case | Result |
| --- | --- |
| Grant idle at the last frame | Not held; it ends by the idle rule at the next sweep. |
| Station offline more than 60 minutes | Touches stop; the grant ends one idle time after the last touch (`coordinator.grant_expired idle`). At the return, its hold row is gone (pruned) and nothing more is written. |
| Deck down, stations up (a Lighting update) | The start sweep holds every grant whose station was heard in the last 60 minutes. |
| Network gap with the station up | The station's outbox keeps mutating calls; at the return the deck decides them with the held grant (real use). Queries fail during the gap, as today. |
| Clock moved back (`last_seen > now`) | Not held. |
| An answer of a link that ended | `resolve_grant_holds` writes nothing (token check). |
| Two answers in a row with a new version | The first one decides; the second sees the stored new version. |
| Unbound grant (a spawn that never bound) | Not held; on a reset or an update with the setting off it is ended too (it is an `active` grant of the station). |
| Owner revokes during the gap | The `UPDATE` of the hold matches only `active`; the revoke wins. |
| Grant past its lifetime during the gap | Not held (`now > expires_at`); `end_stale` ends it. |
| Removal of a station with expired grants | They become `revoked`/`station_removed`, so a later Renew cannot revive them. |

### 3.10 Out of scope

1. A worker grant for a resumed session (P4a W2), owner question Q3.
2. A knob for the hold length (60 minutes fixed), owner question Q2.
3. A "held" badge on the board.
4. The direct-host restart itself (sessions stay `lost` unless auto-continue is on).

## 4. Errors and edge cases of the build

- `store/worker_grant.rs` has 798 lines: no task changes it. `apply_outcome` stays; the audit comes from the wrapper of §3.5.
- `store/schema.rs` has 799 lines: see §3.7 for the line budget.
- `store/mod.rs` has 780 lines: T1 adds one line.
- Tests that count all audit rows after a lifecycle event may see one more row. T1 searches `crates/fleetdeck-deck` for `audit_log(` and fixes such counts in the files it owns; a count in another file is reported to the architect.

## 5. Threat notes and risk class

**High.** The design changes how long a grant authorizes calls and adds three ways that the deck ends a grant. ADR-006 §5.3 changes.

| # | Threat | Defense |
| --- | --- | --- |
| T1 | The hold keeps an unused credential alive longer. | Only while the station of the session has no live link: no query reaches the deck then. Only a bound grant in effect at the last frame. At most 60 minutes after the last frame, then the normal idle rule. `expires_at` stays the cap. The `UPDATE` matches only `active`; Renew stays the only revival. |
| T2 | A coordinator keeps its grant alive by forcing gaps (it restarts its station, or blocks the network). | It needs a process that runs as the owner on that station; such a process can already keep a turn running (G6, accepted). Each gap gives at most one idle time after the return. The lifetime caps it; the owner sees kept rows in the audit log and can revoke. Accepted. |
| T3 | A station lies about its incarnation or version. | The values come only from `station.info` answers on the pinned, authenticated link of that station, and only while that link token lives. They change only the grants of that station's own sessions. A lie can drop them (fail closed) or claim no update (the default keeps them anyway). |
| T4 | A drop leaves a working credential on the station. | The deck refuses at once (`revoked`). The station learns the end like an owner revoke: `session.coordinator_set` with no spec for a grant with a row, `session.coordinator_revoke` through the outbox otherwise. |
| T5 | The hold or the return revives an ended grant. | No statement writes `state = 'active'`. The hold needs `active`, `now <= expires_at` and `in_effect` at the last frame. The return only audits, or revokes. |
| T6 | A race between the hold, an idle check, a revoke and the return. | One database thread. The hold and `end_stale` share the sweep transaction. The return checks the link token and writes in one closure (the G6 pattern). A touch only makes `last_used_at` newer. |
| T7 | A stale `last_seen` holds grants after a long deck outage. | `last_seen` is refreshed every 30 s while a link lives and at its end; a gap longer than 60 minutes holds nothing; `last_seen > now` holds nothing. |
| T8 | A key change carries the grants to a new identity. | A new key is a new station row; no grant moves. The removal of the old row now revokes its grants. A rotation is signed by the old key (`finish_rotation`, `crates/fleetdeck-link/src/revoke.rs`) and keeps them, with kept rows. |
| T9 | The setting weakens security without the owner's consent. | Turning it on needs the elevation (`DeckHolder::danger`); every save has its `settings.deck_set` audit row. Off only drops more. |
| T10 | The audit rows leak data. | Grant id, reason, cause, station name, versions and `offline_ms` only. |
| T11 | Cost. | The sweep: one `SELECT` of the stations and the existing loop over the active grants, every 30 s. The return: one row read and one `UPDATE` per answer (every 60 s per link), plus the holds of that station. The lifecycle audit: two indexed `SELECT`s per lifecycle event (`tokens_by_caller`). |
| T12 | A flapping link floods the audit log. | Holds are made only by the sweep (every 30 s), so a gap shorter than a sweep makes no kept row; at most one kept row per grant and gap. `audit_max_rows` caps the table. |

## 6. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. A worker compiles only (`cargo check -p <crate> --tests`; web: `bun run typecheck` and `bun run lint`) and keeps each file under 800 lines (`wc -l`). The tests run at the merge into `v2` (DL-520).

Order: T1, T3 and T4 own different files and can run in parallel. T2 needs T1 (`grant_audit`, `queue_revoke`, the reason constants) and T3 (`DeckKnobs.keep_grants_on_update`). The adversary DESIGN REVIEW comes before any task starts.

### T1 (backend): the audit rows, the drops by removal, and the end texts (≈ 160 lines of code, ≈ 320 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/grant_audit.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_audit_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/mod.rs` (the line `mod grant_audit;`; nothing else)
  - `crates/fleetdeck-deck/src/store/grants_lifecycle.rs`
  - `crates/fleetdeck-deck/src/store/stations.rs`
  - `crates/fleetdeck-deck/src/store/grant_end.rs`
  - `crates/fleetdeck-deck/src/store/grant_end_tests.rs`
- **Changes:** §3.5 (the module, the wrapper of `record_lifecycle` and `apply_projection`, `queue_revoke` `pub(super)`), §3.4 (removal and rotation), §3.6 item 4 (the two `GrantEnd` variants and texts). Update the doc of `revoke_station` ("its pin, sessions and timeline; the grants of its sessions are revoked (`station_removed`)").
- **Tests** (`grant_audit_tests.rs`, `#[path]` from `grant_audit.rs`; own helpers: an in-memory `DeckStore` with `Det`, pinned stations `st-1` and `st-2`, a grant from `create_grant`, a binding through a `coordinator.bound` event like `grant_scopes_tests.rs:93-108`, a worker grant made like `worker_grant_tests.rs` makes one):
  1. `a_process_exit_that_revokes_a_grant_writes_one_dropped_row`: `coordinator.ended process_exit` of a bound grant without a row → `revoked`; one `coordinator.grant_dropped` row, target the grant id, detail `reason=process_exit;cause=process_exit;station=st-1`.
  2. `a_session_end_that_revokes_a_worker_grant_writes_one_dropped_row`: detail `reason=session_ended;cause=session_ended;station=st-1`.
  3. `a_worker_outcome_names_the_station_reason_as_the_cause`: detail `reason=handoff_skipped;cause=resumed;station=st-1`.
  4. `an_event_that_changes_no_grant_writes_no_row`: a duplicate `coordinator.ended`, a `coordinator.ended` of a revoked grant, a first `coordinator.bound`, a `run.started`: no new row.
  5. `a_snapshot_that_ends_a_grant_writes_a_row_with_the_cause_snapshot`.
  6. `removing_a_station_revokes_the_grants_of_its_sessions`: an active and an expired grant of `st-1` → `revoked`/`station_removed`, two rows `reason=station_removed;station=st-1`; a grant of `st-2` that names `st-1` stays `active`; `take_scope_changes` holds `st-2` when it lost `coordinate`.
  7. `a_key_rotation_keeps_the_grants_and_writes_kept_rows`: `rotate_station` → the grant stays `active`, one `coordinator.grant_kept` row `reason=key_rotated;station=st-1`.
  - `grant_end_tests.rs`: 8. `a_station_update_and_a_station_reset_have_their_own_text` (the exact texts of §3.6 item 4; `EndFacts::at` keeps them after the lifetime; `revoked`/`owner` stays `Revoked`).
- **depends_on:** none.
- **Acceptance (at the merge):** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_audit) | test(grant_end) | test(grants_lifecycle) | test(stations) | test(worker_grant) | test(owner_grants) | test(read_model) | binary(grant_stations) | binary(worker_grant)'`; `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`; `wc -l` of every owned file below 800.

### T3 (backend): the setting `keep_grants_on_update` (≈ 30 lines of code, ≈ 80 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-core/src/config_deck.rs`
  - `crates/fleetdeck-core/src/settings/fields.rs`
  - `crates/fleetdeck-core/src/settings/holder.rs`
  - `crates/fleetdeck-core/src/settings/tests/mod.rs` (the line `mod keep_grants;`)
  - `crates/fleetdeck-core/src/settings/tests/keep_grants.rs` (new)
  - `crates/fleetdeck-deck/src/knobs.rs`
- **Changes:** §3.6 items 1-3.
- **Tests** (`keep_grants.rs`): 1. `keep_grants_on_update_is_on_by_default_and_parses_false`; 2. `the_field_is_a_live_danger_bool_after_fleet_control`; 3. `turning_it_on_needs_the_elevation_and_turning_it_off_does_not`; 4. `the_holder_shows_the_value`. In `knobs.rs` tests: 5. `the_knobs_carry_keep_grants_on_update`.
- **depends_on:** none.
- **Acceptance (at the merge):** `cargo nextest run -p fleetdeck-core --no-fail-fast -E 'test(settings) | test(config)'`; `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(knobs) | binary(board_settings) | binary(board_knobs)'`; `cargo clippy -p fleetdeck-core -p fleetdeck-deck --all-targets -- -D warnings`.

### T2 (backend): the hold and the return (≈ 230 lines of code, ≈ 380 lines of tests)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/grant_hold.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_hold_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_hold_schema.rs` (new)
  - `crates/fleetdeck-deck/src/store/grant_scopes.rs` (the two `#[path]` modules, the call in `sweep_grants`, the module doc bullet of the sweep)
  - `crates/fleetdeck-deck/src/store/schema.rs` (§3.7 only)
  - `crates/fleetdeck-deck/src/link/info.rs` (the call of §3.3 in the `ask` closure)
- **Changes:** §3.2, §3.3, §3.7.
- **Tests** (`grant_hold_tests.rs`; helpers as T1 plus: `up(token)` = `set_reachable(id, true)` + `set_link_features(id, token, vec![])`; `down(token)` = `clear_link_features` + `set_reachable(id, false)`; `heard(id, at)` = `station_heard`; `answer(id, token, inc, version)` = `resolve_grant_holds`):
  1. `a_bound_grant_of_an_offline_station_stays_in_effect_past_its_idle_time`: idle 60 s; bound; heard at t0, `down`; advance 50 s, sweep: `active`, `last_used_at == now`; advance 50 s, sweep: `active`.
  2. `the_hold_ends_sixty_minutes_after_the_last_frame`: offline since t0; sweeps every 50 s to t0 + 60 min: `active`; advance to t0 + 60 min + 1 ms, sweep: no touch; advance 61 s, sweep: `expired`/`idle` with one `coordinator.grant_expired` row.
  3. `a_grant_idle_at_the_last_frame_is_not_held`: idle 60 s; last used t0; heard at t0 + 61 s; `down`; sweep: `expired`/`idle`.
  4. `a_deck_restart_holds_the_grants_of_each_station_heard_in_the_last_hour`: a file database; bound grants of `st-1` (heard t0) and `st-2` (heard t0 − 2 h), idle 30 min, last used t0; close the store; advance 45 min; `DeckStore::open` again: the `st-1` grant is `active` with `last_used_at == now`, the `st-2` grant is `expired`/`idle`.
  5. `an_unbound_grant_or_a_grant_of_an_online_station_is_not_held`; also a grant of `st-2` that names offline `st-1` in `stations`.
  6. `a_station_that_returns_unchanged_keeps_its_held_grants_with_one_kept_row`: hold; `up(1)`; `answer(inc-1, v1)` twice → first `Resolved { kept: 1, dropped: 0 }` with detail `reason=station_offline;station=st-1;offline_ms=<n>`, then `Resolved::default()`; `grant_holds` empty.
  7. `an_update_keeps_the_grants_when_the_setting_is_on`: `answer(inc-1, v1)`, later `answer(inc-1, v2)` → `active`, one kept row `reason=station_updated;station=st-1;from=v1;to=v2`.
  8. `an_update_drops_the_grants_when_the_setting_is_off`: `set_knobs(DeckKnobs { keep_grants_on_update: false, ..Default })`; `v1` then `v2` → `revoked`/`station_updated`, one dropped row; a grant with a `session_owner_grant` row gets a pending `session.coordinator_set` with no spec; a grant without a row gets one outbox `session.coordinator_revoke`; `st-1` loses `coordinate` when no other grant names it; a refused query of that session gets the `StationUpdated` text.
  9. `a_new_incarnation_drops_the_grants_with_either_setting`: `inc-1` then `inc-2` → `revoked`/`station_reset` with the setting on.
  10. `a_station_without_a_version_is_never_updated`: `None` then `None`, setting off → nothing dropped; `v1` then `None` keeps `info_version = v1`.
  11. `the_first_answer_after_the_upgrade_only_records`: `info_incarnation` and `info_version` NULL → stored, nothing kept or dropped.
  12. `an_answer_of_an_ended_link_settles_nothing`: `answer` with a token that is not live → `Resolved::default()`, `info_version` unchanged, the hold row stays.
  13. `the_hold_never_revives_an_ended_grant`: `expired`/`idle`, `expired`/`expired`, `revoked`: unchanged after the sweep and the answer.
  14. `the_sweep_prunes_the_holds_of_ended_grants`.
  15. `a_clock_behind_the_last_frame_holds_nothing`.
- **depends_on:** T1, T3.
- **Acceptance (at the merge):** `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(grant_hold) | test(grant_turns) | test(grant_scopes) | test(station_link) | test(schema) | test(coordinator) | test(link::) | binary(grant_stations) | binary(deck_link) | binary(live_grant_routes)'`; `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`; the guards `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`; `wc -l crates/fleetdeck-deck/src/store/schema.rs` below 800.

### T4 (ux and docs): the board warning, the grant words and the docs (≈ 40 lines of code, ≈ 60 lines of tests, ≈ 30 lines of docs)

- **Owned files:**
  - `web/src/settings/model.ts`
  - `web/src/settings/SettingsFields.tsx`
  - `web/src/settings/fieldCopy.test.tsx` (it tests the bypass hazard of `hazardOf` today)
  - `web/src/fleet/FleetAccess.tsx`
  - `web/src/fleet/grantAccess.test.tsx`
  - `docs/v2/operations.md` (a row after `fleet_control`, `:124`, and two sentences in §7.3)
  - `docs/v2/adr/ADR-006-coordinator-mcp.md` (§3.8 item 6, line 84)
  - `docs/v2/SECURITY-CHECKLIST.md` (row 530)
- **Changes:** §3.6 item 5; the docs. The `operations.md` row: "`keep_grants_on_update` | `true` | When a station comes back after an update or a restart, the fleet grants of its sessions stay. While a station is offline, the deck counts up to 60 minutes as use of those grants. Off: a station that comes back with a new version ends them. A station with new data, or a removed station, always ends them. Applies at once. Turning it on needs the owner's elevation (B26)." The skill `fleet-design-system` applies to the board change.
- **Tests:** 1. `the_keep_grants_switch_shows_a_warning_while_it_is_off` (the exact hazard text; none while on); 2. `a_grant_ended_by_a_station_change_says_why` (the three words of §3.6 item 5).
- **depends_on:** none (the words use the reason strings of §3.5).
- **Acceptance (at the merge):** `bun run test -- settings fleet`; `bun run typecheck`; `bun run lint`.

### At the merge (coordinator)

- `make check` once, after T2.
- The adversary PRE-PR REVIEW of the final branch (high risk), with the acceptance commands above as its fast checks.
- `docs/v2/BACKLOG.md` B26: status done, with the design and the merge commit.

## 7. Decisions made in this draft

1. **The cause is the idle clock, not a key or an incarnation change** (§1.1, §1.4). The fix keeps grants in use across the offline gap; it does not add a revival.
2. **The hold is a touch, like G6** (recorded as use), not a change of `in_effect`. Every idle check stays as it is; the rule survives a deck restart because it reads the durable `last_seen`. Rejected: a frozen idle clock inside `in_effect` (pure and used in about ten places); a touch at the link end only (a deck that stops cannot write it); a revival at the return (ADR-006 forbids it without the owner).
3. **The hold does not depend on the setting.** It only acts while the station cannot reach the deck, and the deck start sweeps before the settings are in the store. The setting decides only what an update does at the return (owner question Q1).
4. **60 minutes fixed** after the last frame (owner question Q2). An upgrade (drain, stop, prepare, activate, start) takes minutes.
5. **Update and reset are seen at each `station.info` answer of a live link,** from two new durable columns. Rejected: the memory map of `station.info` (empty after a deck restart, which is exactly the Lighting update); `stations.incarnation_id` of the read model (a snapshot can write it before or after the answer).
6. **A reset drops with either setting** (fail closed: the station's records may be gone or restored to older ones).
7. **The setting is a top-level `deck.toml` key read through `DeckKnobs`,** so no `DeckLive` literal and no wiring changes.
8. **Two new audit actions** (`coordinator.grant_kept`, `coordinator.grant_dropped`) with `key=value` details, and a row for every lifecycle end observed from a station (the diff of the station's active grants around the event). Rejected: an audit inside `revoke_tx` (it would double the owner revoke rows).
9. **A removal revokes the grants of the removed station's sessions; a rotation keeps them** (owner question Q4).
10. **Worker grants of a resumed session stay out** (P4a W2, owner question Q3).
11. **Risk class high:** adversary DESIGN REVIEW and PRE-PR REVIEW.

## 8. Owner questions

The build proceeds with the defaults.

1. **Q1: Does the hold apply also when "Keep grants on update" is off?** Off then only means "an update ends the grants"; a network gap or a restart without a new version still keeps them. **Default: yes.**
2. **Q2: Is a fixed hold of 60 minutes after the last frame right, without a setting?** **Default: yes (60 minutes, no knob; B27 can add one).**
3. **Q3: Should a worker grant (MCP P4a) come back when its session is resumed after a station restart?** Today a direct-host restart (mac-m4) ends the worker processes, and a resume never gets a worker grant. A fix needs a station record for worker grants, like P4b, and its own review. **Default: no, not in B26; a follow-up backlog item (P4a W2). The audit row shows the drop (`reason=handoff_skipped;cause=resumed`).**
4. **Q4: Does a signed key rotation keep the grants?** **Default: yes (today's rule), with one kept row per grant.**
5. **Q5: With the setting off, are only the grants of the sessions on the updated station ended (not the grants of other stations that name it)?** **Default: yes.**

## Decision log entries (to append)

## DL-530: B26 a station update loses grants through the idle clock, not through a key or an incarnation change (design agent, 2026-10-24)

- Context: B26; design `docs/v2/design/grants-update.md` §1. The owner's live deck and Lighting station (read only, 2026-10-24): no key and no incarnation changed since pairing; no grant ended for an update reason; the reported loss of 2026-10-23 is an idle expiry at 17:07:59 during the update work (last fleet call 16:36:07); the grant of 2026-10-22 on dev1 idled out and its rebind at generation 2 after the resume was ignored, as the "no revival" rule says.
- Decision: the fix keeps the grants of a station's sessions in use while that station is offline (DL-531) and settles them when it returns (DL-532). Nothing revives an ended grant.

## DL-531: B26 a bound grant of an offline station is in use for up to 60 minutes after its last frame (design agent, 2026-10-24)

- Decision: each sweep (also at the deck start) sets `last_used_at = now` for every `active`, bound grant whose station has no live link, whose station was last heard (`stations.last_seen`) at most 60 minutes ago and not after `now`, and that was in effect at that last frame. It records a `grant_holds` row (V51). The step runs after the G6 touch and before `end_stale`, in the sweep transaction. `expires_at` stays the cap. The hold does not read the setting. ADR-006 §5.3 is amended.
- Rejected: an idle predicate in `in_effect`; a touch at the link end only; a revival at the return.

## DL-532: B26 the deck settles a returning station at each `station.info` answer of a live link (design agent, 2026-10-24)

- Decision: the deck stores the incarnation and the version of the last answer (`stations.info_incarnation`, `stations.info_version`, V51). Another incarnation ends every active grant of the station's sessions (`revoked`, `station_reset`), with either setting. Another version ends them (`station_updated`) when `keep_grants_on_update` is off, and keeps them with one `coordinator.grant_kept` row each when it is on. The same incarnation and version keep the held grants with one kept row each. An end queues `session.coordinator_set` with no spec (a grant with a row) or `session.coordinator_revoke` (otherwise). The answer of a link that ended settles nothing. A station without a version is never updated.

## DL-533: B26 the deck setting `keep_grants_on_update` (design agent, 2026-10-24)

- Decision: a top-level `deck.toml` bool, default `true`, `Live`, a danger setting: turning it on needs the elevation, off does not. The store reads it from `DeckKnobs`. Board label "Keep grants on update", in the "Fleet control" section, with the warning "Each station update ends the fleet grants of the sessions on that station. You give them again after the update." while it is off.

## DL-534: B26 the audit log records every grant that a station keeps or drops (design agent, 2026-10-24)

- Decision: new actions `coordinator.grant_kept` and `coordinator.grant_dropped` (actor `deck`, target the grant id, detail `key=value` pairs: `reason`, `cause`, `station`, `from`, `to`, `offline_ms`). Every lifecycle event of a station that ends one of its grants (`coordinator.bound`, `coordinator.ended`, `coordinator.worker_outcome`, `session.ended`, a snapshot) writes one dropped row per ended grant, from the diff of the station's active grants around the event. The owner revoke and the sweep keep their own rows.

## DL-535: B26 a removed station's grants are revoked; a key rotation keeps them (design agent, 2026-10-24)

- Decision: `revoke_station` revokes every grant of the removed station's sessions that is not revoked (`station_removed`) in its transaction, with one dropped row each. `rotate_station` keeps the grants and writes one kept row per active grant (`key_rotated`). A new key is a new station, so no grant moves to it.

## DL-536: B26 the refusal texts of a grant that a station change ended (design agent, 2026-10-24)

- Decision: `GrantEnd::StationUpdated` "Your fleet grant ended when your station was updated, because the deck setting \"Keep grants on update\" is off. The owner can give a new grant on the board." and `GrantEnd::StationReset` "Your fleet grant ended because your station started with new data. The owner can give a new grant on the board." The code stays `forbidden`.

## DL-537: B26 a resumed worker still gets no grant (design agent, 2026-10-24)

- Decision: the P4a rule "a resumed start never gets a grant" stays. A direct-host restart ends the worker processes, and the resume drops their grants with a dropped row (`reason=handoff_skipped;cause=resumed`). A worker grant on resume (P4a W2) is a follow-up item with its own design.

## DL-538: B26 is high risk: adversary design review and pre-PR review (design agent, 2026-10-24)

- Decision: B26 changes how long a grant authorizes calls and adds three ways that the deck ends a grant. It gets the adversary DESIGN REVIEW before T1-T4 start and the PRE-PR REVIEW of the final branch. `SECURITY-CHECKLIST.md` gets row 530. Deck migration V51 (the merge renumbers when needed).
