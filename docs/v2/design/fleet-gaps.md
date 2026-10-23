# Fleet gaps G1, G2, G4 and G5

- **Status:** Draft for the architect, revision 3 (2026-10-23, after the adversary design review rounds 1 and 2; the changes are listed at the end). Risk: **high** (§6: G5 has a deck migration). Adversary DESIGN REVIEW before the design is frozen, PRE-PR REVIEW before the merge.
- **Source:** four product gaps that the architect found on 2026-10-23 while running the backlog program (G1 coordinator spawn outside the roots, G2 `fleet_view` station filter, G4 no feedback for a refused board command, G5 wake key without the session). G3 is a separate track.
- **Code base:** branch `v2`, HEAD `00055138`. All file:line references are on this commit.
- **Live evidence:** read-only queries of the owner's live deck database on Lighting (`~/.local/share/fleetdeck/deck/deck.db`, opened with `sqlite3 -readonly "file:…?mode=ro"`), §4.1. No session was started and no command was sent.

## 1. G1: a coordinator spawn outside the allowed roots ends as "rejected: forbidden"

### 1.1 Verified facts

1. The station checks the folder of a start in the decision transaction. `crates/fleetdeck-station/src/manager/decide.rs:478-484`:
   ```rust
   _ => (
       policy.workspace(&spec.cwd).map_err(|e| match e {
           WorkspaceError::Missing(_) => reject(RejectCode::InvalidCommand, e.to_string()),
           WorkspaceError::Outside(_) => reject(RejectCode::Forbidden, e.to_string()),
       })?,
       None,
   ),
   ```
   The same code runs for `session.start` and `session.import` (`decide.rs:440-449` reads `import.is_some()`; `manager/import_tests.rs:149-154` expects `Forbidden` for an import outside the roots).
2. The text is the `Display` of `WorkspaceError::Outside`: "`` `<canonical path>` is outside the allowed roots of this station``" (`crates/fleetdeck-station/src/manager/policy.rs:28`). It does not name the station. `Policy::name()` (`policy.rs:206-211`) gives the station name (empty for a test policy).
3. The deck cannot check the folder: `StationInfo` carries no allowed roots (`crates/fleetdeck-proto/src/link.rs`, the struct at `:266-298` has `config: Option<ConfigDigest>` only), and only the station can canonicalize the path. So the coordinator call is `accepted` with a pending delivery, and the station decides later.
4. The deck turns the station receipt into the outbox state in `next()` (`crates/fleetdeck-deck/src/store/outbox.rs:271-298`). A rejected receipt gives the reason `format!("rejected: {code}")` (`outbox.rs:281`). The message of the receipt is dropped. The module doc says so (`outbox.rs:17`).
5. `command_status` reads the coordinator receipt and the outbox items of the call: `SELECT outbox_id, state, reason FROM outbox WHERE call_id = ?1` (`crates/fleetdeck-deck/src/store/coordinator_query.rs:260-300`). So the caller sees only `"reason": "rejected: forbidden"`. The full receipt with the message stays in the `commands` table (`outbox.command_id` references it, `crates/fleetdeck-deck/src/store/schema.rs:106`).
6. The same outbox reason is the board text of a mail or a task that was not delivered: `Not delivered: <reason>` (`web/src/fleet/fleet.ts:232`, the reason comes from `o.reason` in `crates/fleetdeck-deck/src/store/fleet.rs:117`, `:135`).
7. A spawn from the owner's board does not use the outbox. `POST /api/commands/session.start` waits up to 5 s for the receipt (`crates/fleetdeck-deck/src/http/command_routes.rs:38`, `:85-90`), and `outcomeOf` shows `Refused: <message>` (`web/src/session/commands.ts:148-171`, used by `web/src/spawn/spawn.ts:634-635`). So the board spawn sheet already shows the station text; only the text itself is weak.
8. `RejectCode` has `#[serde(other)] Unknown` (`crates/fleetdeck-proto/src/command.rs:595-625`). An older peer reads a new code as `unknown` and keeps the message. Only `crates/fleetdeck-deck/src/http/shell_routes.rs:211-219` matches codes, with a `_` arm.

### 1.2 Design

1. **New reject code** `RejectCode::OutsideRoots`, wire name `outside_roots` (the same word as the existing link error code `outside_roots` of `files.v1`, `workspace.precheck` and `artifacts.v1`, `crates/fleetdeck-proto/src/files.rs:50`, `artifacts.rs:51`). Put it after `AlreadyAnswered` and before `Unknown` in `crates/fleetdeck-proto/src/command.rs`:
   ```rust
   /// The folder of a start or an import is outside the allowed roots of
   /// the station (G1, design `fleet-gaps.md` §1); stored.
   OutsideRoots,
   ```
   `stores_receipt()` stays as it is (the new code stores a receipt). Regenerate `web/src/gen/command/RejectCode.ts` with `make ts-types`.
2. **New text**, a free function in `crates/fleetdeck-station/src/manager/policy_limits.rs` (241 lines; `policy.rs` has 785 and `decide.rs` has 792, so they get no new function):
   ```rust
   /// The refusal text of a start or an import whose folder `cwd` is outside
   /// the allowed roots (G1). `station` is the station name; an empty name
   /// (a test policy) reads "this station".
   #[must_use]
   pub fn outside_roots_text(cwd: &str, station: &str) -> String {
       if station.is_empty() {
           format!("The folder {cwd} is outside the allowed roots of this station.")
       } else {
           format!("The folder {cwd} is outside the allowed roots of station {station}.")
       }
   }
   ```
   `cwd` is the folder that the caller sent (`spec.cwd`), not the canonical path: the caller knows the path that it sent.
3. `decide.rs:481` changes to:
   ```rust
   WorkspaceError::Outside(_) => reject(
       RejectCode::OutsideRoots,
       super::policy_limits::outside_roots_text(&spec.cwd, policy.name()),
   ),
   ```
   The `Missing` arm does not change. `resume.rs:147` and `trash.rs:268` keep `Forbidden` (§1.4).
4. **The outbox reason keeps the station text.** In `outbox.rs`, replace the `Some(r) =>` arm of `next()` (`:276-282`) with a call of a new function, and change the module doc line 17 to ``//! - `done` with a stored rejection → `dead` (`rejected: <code>: <message>`);``:
   ```rust
   /// The most characters of the station text in an outbox reason (G1).
   const REASON_TEXT_CHARS: usize = 300;

   /// The reason of a rejected receipt: `rejected: <code>`, then `: ` and
   /// the station text when it has one. Control characters become spaces;
   /// a text above [`REASON_TEXT_CHARS`] is cut and ends with `…`.
   fn rejected_reason(reject: Option<&fleetdeck_proto::Reject>) -> String {
       let Some(j) = reject else {
           return "rejected: unknown".to_owned();
       };
       let code = reject_code(j.code);
       let clean: String = j
           .message
           .trim()
           .chars()
           .map(|c| if c.is_control() { ' ' } else { c })
           .collect();
       if clean.is_empty() {
           return format!("rejected: {code}");
       }
       let mut text: String = clean.chars().take(REASON_TEXT_CHARS).collect();
       if clean.chars().count() > REASON_TEXT_CHARS {
           text.push('…');
       }
       format!("rejected: {code}: {text}")
   }
   ```
   The arm becomes `Some(r) => (OutboxState::Dead, Some(rejected_reason(r.reject.as_ref()))),`. `rejected: no receipt` (`:283`) does not change. Nothing in the code parses an outbox reason (search on HEAD: only tests compare it).
5. **No change** of `command_status`: the `reason` of a delivery now carries the text. Example answer of `command_status` after a coordinator spawn into `/tmp/x` on station `box`:
   ```json
   {"receipt": {"state": "accepted", …},
    "deliveries": [{"outbox_id": "…", "state": "dead",
      "reason": "rejected: outside_roots: The folder /tmp/x is outside the allowed roots of station box."}]}
   ```

### 1.3 Where the owner and the caller see it

| Surface | Before | After |
| --- | --- | --- |
| `command_status` of a coordinator call (the coordinator's chat shows the tool result) | `rejected: forbidden` | `rejected: outside_roots: The folder /tmp/x is outside the allowed roots of station box.` |
| Board spawn sheet (owner start) | ``Refused: `/tmp/x` is outside the allowed roots of this station`` | `Refused: The folder /tmp/x is outside the allowed roots of station box.` |
| Board task and mail rows (`Not delivered: …`) | `Not delivered: rejected: forbidden` (any refusal) | `Not delivered: rejected: <code>: <station text>` |

### 1.4 Compatibility and limits

1. New station, older deck: the deck reads `unknown` and keeps the message; the older outbox reason is `rejected: unknown`. The board spawn sheet shows the new text. Accepted: the deck and the board ship together, and the owner updates the deck first in the normal order.
2. Older station, new deck: the code stays `forbidden`, and the outbox reason now carries the old text (`` rejected: forbidden: `/tmp/x` is outside the allowed roots of this station``). This already answers "why".
3. No link feature flag: a new reject code is data in an existing receipt, and every peer has the `Unknown` fallback.
4. Out of scope: `session.resume` and the worktree trash (`resume.rs:147`, `trash.rs:268`) keep `forbidden` with their texts (their folder was inside the roots when the session started); a board list of the failed calls of a coordinator (§9 Q1).

## 2. G2: `fleet_view` ignores the station filter of the sessions view

### 2.1 Verified facts

1. The station passes `station` for every view: `fleet_view_args` accepts `view`, `cursor` and `station` (`crates/fleetdeck-station/src/coordinator/jsonrpc.rs:120-155`), and `forward.rs:459-464` puts it into `FleetViewRequest`.
2. The deck applies it only to `projects`. `crates/fleetdeck-deck/src/store/fleet_view.rs:289-292`:
   ```rust
   let mut station_filter = match request.view {
       FleetView::Projects => request.station,
       _ => None,
   };
   ```
   The proto doc says the same: "`station` filters the `projects` view and the other views ignore it" (`crates/fleetdeck-proto/src/fleet_view.rs:41-42`). The tool description promises it only for projects: "projects (one station with station)" (`crates/fleetdeck-station/src/coordinator/tools.rs:201`), while the schema says "Station id or name." for every view (`tools.rs:107`).
3. `page_of` keeps an entry when `entry.stations` contains the filter (`fleet_view.rs:181-192`). `build_sessions` and `build_stations` give every entry an empty `stations` list (`crates/fleetdeck-deck/src/store/fleet_view_build.rs:240`, `:162`), so a filter would remove every item. Only `build_projects` fills it (the project's station and its homes, `fleet_view_build.rs:193-198`).
4. **The projects view works:** a name is resolved to the id with `coordinator::names::resolve_station` (`fleet_view.rs:293-303`; exact name, then case-insensitive, unique only, `crates/fleetdeck-deck/src/store/coordinator_names.rs:23-45`). The paging test covers the filter by id (`crates/fleetdeck-deck/src/store/fleet_view_paging_tests.rs:216-239`); no test covers a name.
5. A filter that is neither an id nor a unique name stays as the literal text and matches nothing: the answer is an empty page, without a word (a typo looks like "no sessions").

### 2.2 Design

1. `station` filters the views `stations`, `projects` and `sessions`. `me` ignores it.
2. `fleet_view_build.rs`: `build_stations` gives `vec![record.station_id.clone()]` as the entry stations (`:162`); `build_sessions` gives `vec![session.station_id.clone()]` (`:240`). Update the doc comments of both builders ("The `stations` of an entry is its station, for the `station` filter.").
3. `fleet_view.rs:289-303` changes to:
   ```rust
   // The filter by station (G2): every view but `me`; a name is its id.
   let station_filter = match request.station {
       Some(filter) => {
           let value = filter.as_str().to_owned();
           let id = call(&self.inner.db, move |conn| {
               if let Some(id) = coordinator::names::resolve_station(conn, &value)? {
                   return Ok(Some(id));
               }
               let is_id: bool = conn.query_row(
                   "SELECT EXISTS (SELECT 1 FROM stations WHERE station_id = ?1)",
                   rusqlite::params![value],
                   |r| r.get(0),
               )?;
               Ok(is_id.then_some(value))
           })
           .await?;
           match id {
               Some(id) => Some(StationId::from(id)),
               None => return Err(FleetViewError::Refused(UNKNOWN_STATION.0, UNKNOWN_STATION.1)),
           }
       }
       None => None,
   };
   ```
   The `me` view returns before this code (`fleet_view.rs:271-280`), so it ignores the filter. The filter runs after the cursor check, as today.
4. New refusal constant next to `BAD_PARAMS` (`fleet_view.rs:60`):
   ```rust
   /// The refusal of a `station` filter that is not a station id or a unique
   /// station name (G2).
   pub const UNKNOWN_STATION: (&str, &str) = (
       "invalid_params",
       "no paired station has this id or this unique name",
   );
   ```
5. The module doc of `fleet_view.rs` gets one line after step 6: "The `station` filter (an id or a unique name) keeps the items of one station in the `stations`, `projects` and `sessions` views (G2)."
6. Proto doc `fleet_view.rs:41-42`: "`station` (an id or a unique name) filters the `stations`, `projects` and `sessions` views; `me` ignores it and `cursor`."
7. Tool description (`tools.rs:201`): `"Read the fleet: stations and platforms, projects, live sessions (titles, states), or yourself and your grant. With station (an id or a name), only the items of that station. Pass next_cursor back as cursor."`
8. Paging: the cursor key is the entry key, independent of the filter (`page_of` filters after the cursor position), so paging with a filter works as it does for projects today.

### 2.3 Compatibility

1. The fix is on the deck. A new station with an older deck gets the old answer (every session) with the new tool description. Accepted: the deck is normally updated first.
2. A filter on the `stations` view returned every station; it now returns one. A filter that names no station returned an empty page for projects; it is now `invalid_params` with the text above. Both are corrections.

## 3. G5: the wall-wake key has no session

### 3.1 Verified facts

1. Request ids are per session: the providers number them per session (DL-402: `req-{n}` in the Claude and Codex adapters). The station fixed its own table with V43 (`b0cdbf5f`, `crates/fleetdeck-station/src/store/schema_v43.rs`, key `(session_id, request_id)`).
2. The deck wake scan keys a request by station and request id only. `crates/fleetdeck-deck/src/wake/scan.rs`:
   - `known: BTreeSet<(StationId, String)>` (`:58`), filled at `:148-153`:
     ```rust
     let request_id = request.request_id.as_str().to_owned();
     let key = (station.clone(), request_id.clone());
     present.insert(key.clone());
     if !self.known.insert(key) {
         continue;
     }
     let reserve = store.wake_reserve(station.as_str(), &request_id, now).await;
     ```
   - `present` (`:141`) and `self.known.retain(|k| present.contains(k))` (`:181`).
3. The persisted deduplication table has the same key: `wake_seen (station_id, request_id, first_seen_ms, PRIMARY KEY (station_id, request_id))` with the index `wake_seen_first_seen` (`crates/fleetdeck-deck/src/store/display_v25.rs:43-49`, migration V25). `reserve` inserts `(station_id, request_id, first_seen_ms)` with `ON CONFLICT DO NOTHING` (`crates/fleetdeck-deck/src/store/wake_seen.rs:87-97`); `DeckStore::wake_reserve(station_id, request_id, now_ms)` (`wake_seen.rs:104-122`).
4. Effect: when two sessions of one station ask with the same id (`req-3`), the second request never wakes a wall. In memory the scan skips it (`known`); after a deck restart the row in `wake_seen` gives `Seen`. It fails closed (no wall wakes), so it is a missed wake, not an extra one.
5. The wake job already uses the session: `ReqRef { session, request_id, requested_at }` (`crates/fleetdeck-deck/src/wake/job.rs:21-22`) and the last check reads the open requests of that session (`job.rs:131-145`). Only the scan and the table need the session.
6. Callers of `wake_reserve`: `wake/scan.rs:154`; tests `store/display_gen_tests.rs:493-581` (12 calls and one raw `INSERT` at `:556`), `tests/desktop_wake.rs:345`, `:406`.
7. The deck migration list ends with V47 (`crates/fleetdeck-deck/src/store/schema.rs:787-789`); `schema.rs` has 795 lines (the module limit is 800). The architect gave V48 to wall-control and V49 to this fix. Five integration tests pin the latest deck version 47: `tests/merged_migrations.rs:20`, `merged_migrations_v40.rs:14`, `merged_migrations_idle.rs:11`, `merged_migrations_bg3.rs:39`, `merged_migrations_p0.rs:42`.

### 3.2 Design

1. **Key.** A wake key is `(station_id, session_id, request_id)` in memory and in the table.
2. **Migration V49**, new file `crates/fleetdeck-deck/src/store/wake_seen_schema.rs`, declared at the end of `wake_seen.rs` as:
   ```rust
   #[path = "wake_seen_schema.rs"]
   pub(super) mod schema;
   ```
   (as `worker_grant::tokens_kind_schema`; `store/mod.rs` has 796 lines and gets no new line). Content:
   ```rust
   //! The V49 migration (G5, design `fleet-gaps.md` §3): the wake key has
   //! the session, because request ids are per session (DL-402). An old row
   //! has no session: it is kept as a legacy tombstone with the session
   //! [`LEGACY_SESSION`] (`''`). A tombstone blocks its request id in every
   //! session of its station until it ages out (24 hours, or the eviction of
   //! a saturated table), so a request that an earlier deck reserved never
   //! wakes again (the replay protection of V25 survives the upgrade).

   /// The session of a legacy tombstone (a V48 row). A real session id is
   /// never empty: `wake_reserve` refuses an empty session.
   pub(in crate::store) const LEGACY_SESSION: &str = "";

   /// The V49 migration.
   pub(in crate::store) const SQL: &str = "
   CREATE TABLE wake_seen_v49 (
       station_id     TEXT    NOT NULL,
       session_id     TEXT    NOT NULL,
       request_id     TEXT    NOT NULL,
       first_seen_ms  INTEGER NOT NULL,
       PRIMARY KEY (station_id, session_id, request_id)
   ) STRICT;
   INSERT INTO wake_seen_v49 (station_id, session_id, request_id, first_seen_ms)
       SELECT station_id, '', request_id, first_seen_ms FROM wake_seen;
   DROP TABLE wake_seen;
   ALTER TABLE wake_seen_v49 RENAME TO wake_seen;
   CREATE INDEX wake_seen_first_seen ON wake_seen(first_seen_ms);
   ";
   ```
   `wake_state` (the persisted suppression) does not change. `DROP TABLE` also drops the old index; the index is created again after the rename. The V25 table holds at most 10 000 rows (`WAKE_SEEN_MAX`), so the copy is small. No view or trigger refers to `wake_seen` (V25 creates none). Reversal: create the V25 shape and copy `(station_id, request_id, MIN(first_seen_ms))` grouped by `(station_id, request_id)`.
3. **List entry.** Append one line at the end of the `migrations()` list in `schema.rs` (after the V48 line of wall-control):
   ```rust
   M::up(super::wake_seen::schema::SQL), // V49 (G5): the wake key has the session.
   ```
   One line only: `schema.rs` is at 795 lines and wall-control also adds to it.
4. **Store.** `wake_seen.rs`:
   - `fn reserve(conn, station_id: &str, session_id: &str, request_id: &str, now: i64)`:
     1. First, before the transaction: `if session_id == schema::LEGACY_SESSION { return Ok(WakeReserve::Error); }` (an empty session is the tombstone marker, never a key; it never wakes).
     2. The suppression, the 24-hour deletion and the eviction do not change. They also apply to the tombstones: a tombstone ages out as every other row.
     3. The insert becomes `INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING` → `n`.
     4. Then, in the same transaction: `let legacy: bool = tx.query_row("SELECT EXISTS (SELECT 1 FROM wake_seen WHERE station_id = ?1 AND session_id = '' AND request_id = ?2)", params![station_id, request_id], |r| r.get(0))?;` (the primary key serves the lookup).
     5. `tx.commit()?;` The answer is `Reserved` only when `n == 1 && !legacy`; else `Seen`. The new key is inserted also when a tombstone matches, so the key stays `Seen` after the tombstone ages out.
   - `pub async fn wake_reserve(&self, station_id: &str, session_id: &str, request_id: &str, now_ms: i64) -> WakeReserve`. Doc: "Records that the wake rule observed `(station_id, session_id, request_id)`. A legacy tombstone of `(station_id, request_id)` (V49) makes the answer `Seen`; an empty `session_id` is `Error`."
   - Module doc: add after line 20: "//! The key is `(station_id, session_id, request_id)` (V49, G5): request ids are per session. A V48 row is a legacy tombstone (session `''`): it makes its request id `Seen` in every session of its station until it ages out." Step 5 of the module doc becomes "5. `INSERT ... ON CONFLICT DO NOTHING`, then the tombstone check: one row inserted and no tombstone is `Reserved`; else `Seen`."
5. **Scan.** `scan.rs`:
   - `/// A request on a card: the station, the session and the request id (G5).` `type RequestKey = (StationId, SessionId, String);`
   - `known: BTreeSet<RequestKey>`; `present: BTreeSet<RequestKey>`.
   - At `:148-154`:
     ```rust
     let request_id = request.request_id.as_str().to_owned();
     let session = &card.summary.session_id;
     let key = (station.clone(), session.clone(), request_id.clone());
     present.insert(key.clone());
     if !self.known.insert(key) {
         continue;
     }
     let reserve = store
         .wake_reserve(station.as_str(), session.as_str(), &request_id, now)
         .await;
     ```
   - `Candidate.session` takes `session.clone()` (same value as today).
6. **No other change.** The cooldown key `(station, monitor)` and the job are correct.

### 3.3 Upgrade behavior

| Case | Result |
| --- | --- |
| The deck starts on V49 | Every V48 row is a legacy tombstone `(station_id, '', request_id, first_seen_ms)`. The first scan is a baseline as before (`scan.rs:130-140`, `self.first`). |
| A request was reserved before the upgrade, and the station restores it after the baseline (a new incarnation whose snapshot does not have it, then an `ApprovalRequested` event; the case of `tests/desktop_wake.rs:339-369`) | The tombstone of `(station, request_id)` gives `Seen`: no wake, also inside the 60 s window. This is the V25 replay protection, kept through the upgrade (design review r1 BLOCKER). |
| A new session asks with a provider id (`req-3`) that has a tombstone, within 24 hours after the upgrade | `Seen`: no wake. This is the behavior of HEAD (fail closed). It ends when the tombstone ages out (24 hours after its `first_seen_ms`) or a saturated table evicts it. Accepted (§8 item 7). |
| A tombstone ages out after a key was recorded under it | The recorded key `(station, session, request_id)` stays and gives `Seen`; another session with that id gives `Reserved`. |
| Two sessions ask with the same id at the same time (no tombstone) | Two keys, two reservations, one job for the monitor (the candidates join), one wake. |
| The saturation state is set at the upgrade | It is kept (`wake_state` unchanged). |

## 4. G4: the board shows nothing when a station refuses a board command

### 4.1 Verified facts and a correction to the report

1. **The HTTP post does not only queue.** `POST /api/commands/<method>` stores the command, then waits up to `RECEIPT_WAIT` = 5 s for the receipt and answers with the command as it is then (`crates/fleetdeck-deck/src/http/command_routes.rs:38`, `:85-90`; `DeckLink::wait_command`, `crates/fleetdeck-deck/src/link/router.rs:551-572`). The receipt with its reject code and message is in that answer.
2. `outcomeOf` turns a `done` command with a rejected receipt into `{ ok: false, text: 'Refused: <message>' }`; `pending` into `{ ok: true, text: 'Sent. The station has not confirmed it yet.' }` (`web/src/session/commands.ts:148-171`). `sessionActions.run` returns it (`commands.ts:232-252`).
3. Live evidence (read only, the owner's deck): every refused `approval.answer` settled in milliseconds, so every one of the owner's clicks got the refusal back in the HTTP answer.

   | `state` | `reject.code` | count | first (UTC) | last (UTC) | mean ms to settle | max ms |
   | --- | --- | --- | --- | --- | --- | --- |
   | done | (accepted) | 56 | 2026-10-15 11:06:38 | 2026-10-23 16:20:09 | 141.9 | 6644 |
   | done | `unknown_request` | 76 | 2026-10-20 12:21:03 | 2026-10-23 16:31:01 | 14.4 | 45 |

   All 76 refusals have origin `board`, author `owner`, message "no such request". The clicks of 2026-10-23 16:30:23-16:31:01 are for one request (`req-3` of session `01M5MSG6…`), up to three clicks in one second. The cause of the refusal is DL-402 (fixed on the station by V43).
4. Where the refusal is shown today:
   - Session view: the approval dock calls `controller.act('answer', …)` (`web/src/session/SessionView.tsx:558-563`). The controller keeps one `outcome` (`web/src/session/controller.ts:72-94`), and only the session header renders it (`web/src/session/SessionHeader.tsx:184`, `:750-761`), at the top of the pane. The dock is at the foot (`SessionView.tsx:546-564`, `fd-pane-foot`).
   - A second identical refusal sets an equal outcome: the alert does not change. So repeated clicks show no visible change.
   - Needs-you rail: shows the refusal under the request (`web/src/board/NeedsYouRail.tsx:111-124`, `:207`, `:286-289`; e2e `web/e2e/session.spec.ts` "a refused answer from the rail shows its text").
   - Wall and watch tiles: `setNote(o.text)` (`web/src/watch/WatchPage.tsx:262-275`).
5. **Inference** (the owner's screen was not recorded): the owner answered in the session dock and did not see the alert in the header, and the repeated clicks gave the same alert. This matches facts 3 and 4.
6. Two real gaps remain in the code:
   - A command that the station does not answer in 5 s stays "Sent. The station has not confirmed it yet." with `ok: true`. The board never reads the command again. `GET /api/commands/{command_id}` exists (`command_routes.rs:507-528`, route `crates/fleetdeck-deck/src/http/mod.rs:549-550`; a member reads its own commands only) and is not used by the board (search on HEAD).
   - The rail keys its pending set and its errors by `request_id` only (`NeedsYouRail.tsx:86-87`, `:105-124`, `:206-207`): two sessions with `req-3` share one pending state and one error (the DL-402 collision on the board).
7. `sessionActions` already receives a `get` function in the running board (`web/src/runtime/RuntimeApp.tsx:139-149`); it is `null` in tests by default (`commands.ts:225-231`).

### 4.2 Design

The smallest sound mechanism uses the existing route: the board reads `GET /api/commands/<id>` after a `pending` answer. No new route, no event, no deck change.

#### 4.2.1 `web/src/session/commands.ts`

1. `Outcome` gets an optional follow-up:
   ```ts
   export type Outcome = {
     readonly ok: boolean;
     readonly text: string;
     /**
      * G4: set when the deck answered `pending`. Such an outcome is provisional: `ok` says only that
      * the deck stored the command. It reads the command until the station decides and resolves with
      * that outcome, or with `null` when it stops (aborted, 120 s, 3 failed reads). It never rejects.
      */
     readonly later?: (signal: AbortSignal) => Promise<Outcome | null>;
   };
   ```
   **The contract (revision 2):** an outcome with `later` is not final. A consumer that acts on success (marks, unlocks, closes a dialog, uses up an input, counts, says "done") must wait for the final outcome; a consumer that only shows the text must show a final refusal when it comes. §4.2.5 lists every consumer and its decision. Revision 3: this applies also to a success side effect that runs **inside** the call that a consumer gives to `controller.act` (the shell chips, §4.2.3 item 2): the controller follows the outcome that the call returns, not the code that ran before the return.
2. New constants and function (in `commands.ts`, after `outcomeOf`):
   ```ts
   /** G4: the waits before each read of a pending command: 1 s for the first 10 reads, then 5 s. */
   export const FOLLOW_FAST_MS = 1_000;
   export const FOLLOW_FAST_READS = 10;
   export const FOLLOW_SLOW_MS = 5_000;
   /** G4: the board stops following a command 120 s after the first wait. */
   export const FOLLOW_LIMIT_MS = 120_000;
   /** G4: the board stops after this many failed reads in a row. */
   export const FOLLOW_MAX_ERRORS = 3;

   const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));

   /** G4: reads `GET /api/commands/<id>` until the command is not `pending`; `null` when it stops. */
   export async function followCommand(
     get: Get,
     id: string,
     signal: AbortSignal,
     wait: (ms: number) => Promise<void> = sleep,
     now: () => number = Date.now,
   ): Promise<Outcome | null> {
     const start = now();
     let errors = 0;
     for (let n = 0; ; n += 1) {
       await wait(n < FOLLOW_FAST_READS ? FOLLOW_FAST_MS : FOLLOW_SLOW_MS);
       if (signal.aborted || now() - start > FOLLOW_LIMIT_MS) return null;
       let v: unknown;
       try {
         v = await get(`/api/commands/${encodeURIComponent(id)}`);
         errors = 0;
       } catch {
         errors += 1;
         if (errors >= FOLLOW_MAX_ERRORS) return null;
         continue;
       }
       if (signal.aborted) return null;
       if (record(v) && v['state'] === 'pending') continue;
       return outcomeOf(v);
     }
   }

   /** G4: the final outcome of `o`: `o` when it has no follow-up, else the follow-up's outcome, or
    *  `null` when the follow stops. */
   export async function finalOutcome(o: Outcome, signal: AbortSignal): Promise<Outcome | null> {
     if (o.later === undefined) return o;
     return o.later(signal).catch(() => null);
   }

   /** G4 (revision 3): runs `done` once `o` is a confirmed success: now for a final `ok`, else when
    *  its follow-up resolves `ok` and the follow was not aborted. A refusal or a stopped follow
    *  (`null`) never runs it. Returns the outcome to hand on: `o` itself when it is final, else `o`
    *  with a wrapped follow-up (the same final outcome). */
   export function onConfirmed(o: Outcome, done: () => void): Outcome {
     const later = o.later;
     if (later === undefined) {
       if (o.ok) done();
       return o;
     }
     return {
       ...o,
       later: async (signal) => {
         const final = await later(signal);
         if (final !== null && final.ok && !signal.aborted) done();
         return final;
       },
     };
   }
   ```
   `onConfirmed` does not follow by itself: the consumer that receives the returned outcome follows it (for the composer: `controller.act`, §4.2.2). When nobody follows (the owner opened another session before the first answer came, `controller.ts` returns before it reads `later`), `done` never runs: an unconfirmed success side effect does not happen.
3. `run` (`commands.ts:244`) changes to:
   ```ts
   const reply = await post(`/api/commands/${METHODS[command.op]}`, body);
   const outcome = outcomeOf(reply);
   const pendingId =
     record(reply) && reply['state'] === 'pending' && typeof reply['command_id'] === 'string'
       ? reply['command_id']
       : null;
   return pendingId !== null && get !== null
     ? { ...outcome, later: (signal) => followCommand(get, pendingId, signal) }
     : outcome;
   ```
   `outcomeOf` stays pure (no `later`). `spawn.ts`, `pairing.ts` and the other callers of `outcomeOf` do not change.

#### 4.2.2 `web/src/session/controller.ts`

Revision 2: the controller follows **each** pending command until it ends or the session view ends (design review r1, MAJOR 1). A newer action never aborts an older follow. A refused answer is kept **per request** for the dock, so two refusals for two requests are both shown. The count of a repeat compares a final outcome with the previous **final** outcome, not with a pending text (design review r1, MINOR).

1. New types and pure helpers (the state changes are pure functions, so `bun:test` tests them without a DOM):
   ```ts
   /** The outcome that the controller shows (G4): the action that gave it, the request of an
    * answer, and how many times in a row the same action on the same target gave the same text. */
   export type Shown = Outcome & {
     readonly action?: string;
     readonly target?: string | null;
     readonly repeats?: number;
   };

   /** G4: the next shown outcome; an equal action, target, `ok` and text counts one more repeat. */
   export function nextShown(
     prev: Shown | null,
     action: string,
     target: string | null,
     outcome: Outcome,
   ): Shown {
     const same =
       prev !== null &&
       prev.action === action &&
       (prev.target ?? null) === target &&
       prev.ok === outcome.ok &&
       prev.text === outcome.text;
     return { ok: outcome.ok, text: outcome.text, action, target, repeats: same ? (prev.repeats ?? 1) + 1 : 1 };
   }

   /** G4: the words of a shown outcome: ` (n times)` after a repeat. */
   export function shownText(o: Shown): string {
     return (o.repeats ?? 1) > 1 ? `${o.text} (${o.repeats} times)` : o.text;
   }

   /** G4: the refused answers of the session, by request id (the dock shows them). */
   export type Refusals = Readonly<Record<string, Shown>>;

   /** G4: what the controller shows. `outcome` is the header line; `settled` is the last final
    *  outcome (the base of the repeat count); `refusals` are the refused answers by request. */
   export type ShownState = {
     readonly outcome: Shown | null;
     readonly settled: Shown | null;
     readonly refusals: Refusals;
   };
   export const EMPTY_SHOWN: ShownState = { outcome: null, settled: null, refusals: {} };

   /** A final outcome: it becomes `settled`; an answer updates its request's refusal; `show` puts
    *  it on the header line. */
   function settle(s: ShownState, action: string, target: string | null, o: Outcome, show: boolean): ShownState {
     const shown = nextShown(s.settled, action, target, o);
     let refusals = s.refusals;
     if (action === 'answer' && target !== null) {
       const { [target]: prev, ...rest } = s.refusals;
       refusals = o.ok ? rest : { ...rest, [target]: nextShown(prev ?? null, action, target, o) };
     }
     return { outcome: show ? shown : s.outcome, settled: shown, refusals };
   }

   /** G4: the state after the first outcome of an action. A provisional outcome (with `later`)
    *  goes on the header line only: it is not counted and does not touch the refusals. */
   export function onFirst(s: ShownState, action: string, target: string | null, o: Outcome): ShownState {
     if (o.later !== undefined) return { ...s, outcome: nextShown(null, action, target, o) };
     return settle(s, action, target, o, true);
   }

   /** G4: the state after the final outcome of a followed action whose first shown outcome is
    *  `first`. A refusal always shows; a success shows only while `first` is still shown. */
   export function onFinal(
     s: ShownState,
     first: Shown | null,
     action: string,
     target: string | null,
     o: Outcome,
   ): ShownState {
     return settle(s, action, target, o, !o.ok || s.outcome === first);
   }

   /** G4: the dock lines: the refusal text of each open request that has one. */
   export function dockRefusals(
     refusals: Refusals,
     open: readonly OpenRequest[],
   ): Readonly<Record<string, string>> {
     const lines: Record<string, string> = {};
     for (const r of open) {
       const o = refusals[r.request_id];
       if (o !== undefined) lines[r.request_id] = shownText(o);
     }
     return lines;
   }

   /** G4: the header outcome: not a refused answer that the dock shows (it is not shown twice). */
   export function headerOutcome(
     c: Pick<Controller, 'outcome' | 'refusals'>,
     open: readonly OpenRequest[],
   ): Shown | null {
     const o = c.outcome;
     if (o === null || o.ok || o.action !== 'answer' || o.target == null) return o;
     const t = o.target;
     return c.refusals[t] !== undefined && open.some((r) => r.request_id === t) ? null : o;
   }

   /** G4: the dock's answer handler; it names the request as the target of the action. */
   export const answerVia =
     (c: Pick<Controller, 'act'>, actions: SessionActions | null | undefined) =>
     (request: string, option: string, reply: Reply | null) => {
       if (actions) c.act('answer', () => actions.answer(request, option, reply), request);
     };
   ```
   `headerOutcome` returns early for a `null` or an `ok` outcome before it reads `refusals` (test doubles that cast a partial controller with `as never` have no `refusals`).
2. `Controller`: `outcome: Shown | null`; new `readonly refusals: Refusals;`; `act(name: string, call: () => Promise<Outcome>, target?: string): void`; `dismiss(): void`.
3. `useSessionController`:
   - Replace the `outcome` state by `const [shown, setShown] = useState<ShownState>(EMPTY_SHOWN)` and `const shownRef = useRef<ShownState>(EMPTY_SHOWN)`, with one function `apply(f: (s: ShownState) => ShownState): ShownState` that computes `f(shownRef.current)`, stores it in `shownRef.current`, calls `setShown` with it and returns it.
   - `const follows = useRef(new Set<AbortController>())`.
   - The render-time reset on a new `sessionId` (`controller.ts:68-72`) calls `setShown(EMPTY_SHOWN)` in place of `setOutcome(null)`.
   - The existing `useEffect` on `sessionId` becomes:
     ```ts
     useEffect(() => {
       current.current = sessionId;
       shownRef.current = EMPTY_SHOWN;
       const all = follows.current;
       // G4: every follow of this session ends with the session view or a session change.
       return () => {
         for (const c of all) c.abort();
         all.clear();
       };
     }, [sessionId]);
     ```
   - `act(name, call, target)`:
     ```ts
     const started = current.current;
     setBusy(name);
     void (async () => {
       let result: Outcome;
       try {
         result = await call();
       } catch (error) {
         const message = error instanceof Error ? error.message : String(error);
         result = { ok: false, text: `Not delivered: ${message}` };
       }
       // The owner opened another session meanwhile: the result is not theirs to see.
       if (current.current !== started) return;
       setBusy(null);
       const t = target ?? null;
       const first = apply((s) => onFirst(s, name, t, result)).outcome;
       if (result.later === undefined) return;
       // G4: each pending command has its own follow; a newer action does not end it.
       const ctl = new AbortController();
       follows.current.add(ctl);
       const final = await result.later(ctl.signal).catch(() => null);
       follows.current.delete(ctl);
       if (final === null || ctl.signal.aborted || current.current !== started) return;
       apply((s) => onFinal(s, first, name, t, final));
     })();
     ```
   - `dismiss` becomes `() => apply((s) => ({ ...s, outcome: null, settled: null }))` (the header line goes; the dock lines stay until their request closes or the answer succeeds).
   - The controller returns `outcome: shown.outcome` and `refusals: shown.refusals`.
   - `busy` is `null` while the board follows: the owner can act again, and every earlier follow continues.

#### 4.2.3 The surfaces

1. `web/src/session/ApprovalDock.tsx`: new optional prop `readonly refusals?: Readonly<Record<string, string>> | undefined;` (the lines of `dockRefusals`). In each `<li>`, as its last child:
   ```tsx
   {props.refusals?.[r.request_id] === undefined ? null : (
     <p className="fd-q-error" role="alert">
       {props.refusals[r.request_id]}
     </p>
   )}
   ```
   `.fd-q-error` exists (`web/src/styles/home.css:461-466`, loaded globally by `web/src/main.tsx:23`); it is the rail's refusal line. No new CSS, token or class.
2. `web/src/session/SessionView.tsx` (797 lines; the change must not add lines):
   - `:40` `import { answerVia, dockRefusals, useSessionController } from './controller';`
   - `:560-563` (the four-line `onAnswer`) becomes `onAnswer={answerVia(controller, actions)}`.
   - After `busy={busy !== null}` (`:559`) add `refusals={dockRefusals(controller.refusals, s.open_requests)}`.
   - Revision 3, the shell chips (design review r2 BLOCKER). `:37` becomes `import { onConfirmed, type SessionActions } from './commands';`. In `onSend`, the comment at `:597` becomes `// B8: the first 64 chips go with the prompt; a confirmed prompt uses them up (G4).`, and the two lines `:612-613` (`if (outcome.ok) shellChips.consume(results);` and `return outcome;`) become one line:
     ```ts
     return onConfirmed(outcome, () => shellChips.consume(results));
     ```
     So an immediate `Done.` consumes the chips as today; a pending prompt keeps them until the follow of `controller.act('send')` resolves `ok`; a refusal, a stopped follow or a session change keeps them, and the next prompt names them again. Sending a result twice is safe: the station carries each result once (`shell_context.rs:1-15`; `attachable` selects only rows with `included_in IS NULL`, `store/shell_runs.rs:379-388`), so a result that an earlier, later-accepted prompt carried is skipped.
   - Line count: the four-line `onAnswer` becomes one (−3), the `refusals` prop (+1), the chips (−1): 794 lines.
3. `web/src/session/SessionHeader.tsx`:
   - `:184` becomes `const outcome = headerOutcome(controller, s.open_requests);` (a refusal that the dock shows is not shown twice).
   - `:753` becomes `<span>{shownText(outcome)}</span>`.
   - Import `headerOutcome` and `shownText` from `./controller`.
4. `web/src/board/NeedsYouRail.tsx` (each answer has its own `.then` chain, so each pending command is followed on its own already):
   - New exported pure functions:
     ```ts
     /** G4: the rail key of a request: station, session and request id (DL-402). */
     export const railKey = (key: SessionKey, requestId: string): string =>
       `${key.station_id}/${key.session_id}/${requestId}`;

     /** G4: the rail errors after an outcome of the answer `k`: a refusal counts against the last
      *  refusal of `k`; a success or a provisional outcome changes nothing. */
     export function railErrors(
       errors: Readonly<Record<string, Shown>>,
       k: string,
       o: Outcome,
     ): Readonly<Record<string, Shown>> {
       if (o.ok) return errors;
       return { ...errors, [k]: nextShown(errors[k] ?? null, 'answer', k, o) };
     }
     ```
   - `pending` holds rail keys; `errors` becomes `Readonly<Record<string, Shown>>` keyed by rail key.
   - `answer(actions, key: SessionKey, request, optionId)`: `const k = railKey(key, request.request_id)`; on the outcome: `settle(k)`; `setErrors((e) => railErrors(e, k, outcome))`; when `outcome.later` is set: `void outcome.later(signal()).then((final) => { if (final !== null) setErrors((e) => railErrors(e, k, final)); });`. The rejection arm `(e: unknown) => …` becomes `settle(k); setErrors((all) => railErrors(all, k, { ok: false, text: e instanceof Error ? e.message : 'Request failed' }));` (the same text as today).
   - `life`: `const life = useRef<AbortController | null>(null); useEffect(() => { const c = new AbortController(); life.current = c; return () => c.abort(); }, []);` and `const signal = () => life.current?.signal ?? AbortSignal.abort();`.
   - The keyboard path (`:149-154`) uses `pending.has(railKey(item.key, item.request.request_id))` and calls `answer(actions, item.key, item.request, option.option_id)`; the render (`:206-207`) uses `railKey(w.key, r.request_id)` for `busy` and `error`, and the error line renders `shownText(error)`.
5. `web/src/watch/WatchPage.tsx:262-275`: after `setNote(o.ok ? null : o.text)`, when `o.later` is set: `void o.later(signal()).then((final) => { if (final !== null && !final.ok) setNote(final.text); });` with the same `life` and `signal()` pattern as the rail.
6. `web/src/session/ExecutionDialog.tsx` ("Mark as ended", design review r1 MAJOR 2):
   - New exported pure function and text:
     ```ts
     /** G4: the text when the station did not confirm the mark while the board followed it. */
     export const MARK_UNCONFIRMED =
       'The station did not confirm the mark in time. Check the processes again.';

     export type MarkResult = { readonly marked: true } | { readonly marked: false; readonly text: string };

     /** G4: "Mark as ended": the command, followed while it is pending. Only a confirmed mark marks. */
     export async function markEnded(
       actions: Pick<SessionActions, 'reconcileExecution'>,
       shimId: string,
       signal: AbortSignal,
       onPending: (text: string) => void,
     ): Promise<MarkResult> {
       const sent = await actions.reconcileExecution(shimId);
       if (sent.later !== undefined) onPending(sent.text);
       const final = await finalOutcome(sent, signal);
       if (final === null) return { marked: false, text: MARK_UNCONFIRMED };
       return final.ok ? { marked: true } : { marked: false, text: final.text };
     }
     ```
   - New state `const [markNote, setMarkNote] = useState<string | null>(null);` and `const life = useRef<AbortController | null>(null);`. The existing `alive` effect (`:81-86`) also makes `const c = new AbortController(); life.current = c;` and its cleanup also calls `c.abort()`.
   - `mark` (`:108-116`) becomes:
     ```ts
     const mark = async (shimId: string) => {
       setBusy(true);
       setMarkError(null);
       const result = await markEnded(actions, shimId, life.current?.signal ?? AbortSignal.abort(), (text) => {
         if (alive.current) setMarkNote(text);
       });
       if (!alive.current) return;
       setBusy(false);
       setMarkNote(null);
       if (result.marked) setMarked(true);
       else setMarkError(result.text);
     };
     ```
   - Render: under the `Mark as ended` button, before the `markError` line: `{markNote === null ? null : <p className="fd-note" role="status">{markNote}</p>}`. The button keeps `Marking…` while the board follows.
7. `web/src/terminal/handover.ts` `returnToBoard` (`:85-112`): the return type becomes `Promise<Outcome>`, and the end (`:108-111`) becomes:
   ```ts
   const resumed = await deps.resume();
   const back = (o: Outcome): Outcome =>
     o.ok
       ? { ok: true, text: 'Back under board control.' }
       : { ok: false, text: `Returned, but the resume failed: ${o.text}` };
   const later = resumed.later;
   if (later === undefined) return back(resumed);
   // G4: a pending resume keeps its follow-up; the final text is the same as without the wait.
   return {
     ...back(resumed),
     text: 'Back under board control. The station has not confirmed the resume yet.',
     later: async (signal) => {
       const final = await later(signal);
       return final === null ? null : back(final);
     },
   };
   ```
   `handoverFlow.back` returns it as is; the session view runs it through `controller.act('return', …)` (`SessionView.tsx:224`), which follows it.
8. `web/src/App.tsx` `returnTerminal` (`:399-402`; the tab strip's "Return", outside the session view): the success arm becomes
   ```ts
   (outcome) => {
     notify({ text: outcome.text, tone: outcome.ok ? 'info' : 'error' }, route);
     // G4: a refused resume after a pending one is shown too (the follow ends by itself after 120 s).
     void outcome.later?.(new AbortController().signal).then((final) => {
       if (final !== null && !final.ok) notify({ text: final.text, tone: 'error' }, route);
     });
   },
   ```
   `App.tsx` has 789 lines; this adds 5 (794, under the 800-line limit).
9. `web/src/chat/ShellCard.tsx` `ReleaseDialog` ("Release the folder", revision 3, design review r2 BLOCKER). On HEAD `release` (`:107-115`) calls `onClose()` on any `ok`, so a pending release closes the dialog and a later refusal is never shown (the prop is `env.onRelease` = `actions.releaseShell`, `SessionView.tsx:507`).
   - New exported text and function (import `finalOutcome` with `type Outcome` from `../session/commands`):
     ```ts
     /** G4: the text when the station did not confirm the release while the board followed it. */
     export const RELEASE_UNCONFIRMED =
       'The station did not confirm the release in time. Check the command card again.';

     /** G4: the release, followed while it is pending: the final outcome, or a refusal with
      *  RELEASE_UNCONFIRMED when the follow stops. Only an `ok` result closes the dialog. */
     export async function releaseFollowed(
       onRelease: () => Promise<Outcome>,
       signal: AbortSignal,
       onPending: (text: string) => void,
     ): Promise<Outcome> {
       const sent = await onRelease();
       if (sent.later !== undefined) onPending(sent.text);
       return (await finalOutcome(sent, signal)) ?? { ok: false, text: RELEASE_UNCONFIRMED };
     }
     ```
   - New state `const [note, setNote] = useState<string | null>(null);` and `const life = useRef<AbortController | null>(null);`. The existing `alive` effect (`:101-106`) also makes `const c = new AbortController(); life.current = c;`, and its cleanup also calls `c.abort()`.
   - `release` becomes:
     ```ts
     const release = async () => {
       setBusy(true);
       setRefusal(null);
       const outcome = await releaseFollowed(
         onRelease,
         life.current?.signal ?? AbortSignal.abort(),
         (text) => {
           if (alive.current) setNote(text);
         },
       );
       if (!alive.current) return;
       setBusy(false);
       setNote(null);
       if (outcome.ok) onClose();
       else setRefusal(outcome.text);
     };
     ```
   - Render: after the `refusal` line, `{note === null ? null : <p className="fd-note" role="status">{note}</p>}` (`.fd-note` is global, `web/src/styles/base.css:1514`). The button keeps `Releasing…` while the board follows. Cancel, Escape and the overlay click still close the dialog; closing ends the follow (the cleanup aborts it), and the card shows the result (`Released`, or the unchanged card with "Release the folder").
10. `web/src/board/FleetBoard.tsx` `OfflineTools` "Resume all" (revision 3: the audit found it; it counts a pending resume as resumed, `:222-238`).
   - New exported pure functions (above `OfflineTools`):
     ```ts
     /** G4: resumes each session in turn. A resume counts as confirmed only with a final `ok`; a
      *  pending one counts apart (its card shows when it resumes). A failure counts as neither. */
     export async function resumeEach(
       targets: readonly (Pick<SessionActions, 'resume'> | null)[],
     ): Promise<{ readonly confirmed: number; readonly pending: number }> {
       let confirmed = 0;
       let pending = 0;
       for (const actions of targets) {
         try {
           const o = await actions?.resume();
           if (o?.ok === true) {
             if (o.later === undefined) confirmed += 1;
             else pending += 1;
           }
         } catch {
           // A failed resume counts as not resumed.
         }
       }
       return { confirmed, pending };
     }

     /** G4: the report of "Resume all". */
     export function resumeReport(confirmed: number, pending: number, total: number): string {
       const head = `Resumed ${confirmed} of ${total}`;
       return pending === 0 ? head : `${head}. ${pending} not confirmed yet`;
     }
     ```
   - `resumeAll` keeps `targets`, `setRunning` and the timer; the loop and the report become `const { confirmed, pending } = await resumeEach(targets);` and `setReport(resumeReport(confirmed, pending, targets.length));`. "Resume all" does not follow: the board does not hold the button for up to 120 s per session, and each card shows its own state.

#### 4.2.4 UI text

| Case | Text | Where |
| --- | --- | --- |
| Refused answer, request still open | `Refused: <station message>` | dock, under the request (`role="alert"`) |
| The same refusal again | `Refused: <station message> (2 times)`, `(3 times)`, … | dock or rail, the same line |
| Any other refusal | `Refused: <station message>` (unchanged) | session header |
| Pending | `Sent. The station has not confirmed it yet.` (unchanged) | session header |
| Pending, then refused | the refusal shows also when the owner did another action meanwhile; an answer refusal shows under its request, another refusal replaces the header line | dock (answer) or header |
| Two pending answers for two requests, both refused (in any order) | each request shows its own refusal | dock |
| Pending, then refused, again the same | `Refused: <station message> (2 times)` | dock or rail |
| Pending, then accepted | `Done.` replaces the pending text, only while the pending text is still shown | header |
| "Mark as ended", pending | `Sent. The station has not confirmed it yet.` under the button; the button reads `Marking…` | execution dialog |
| "Mark as ended", pending, then refused | `Refused: <station message>` (`role="alert"`); no "Marked as ended" | execution dialog |
| "Mark as ended", the follow stops | `The station did not confirm the mark in time. Check the processes again.` | execution dialog |
| "Return" with a pending resume | `Back under board control. The station has not confirmed the resume yet.`; then `Back under board control.` (header) or, on a refusal, `Returned, but the resume failed: Refused: <station message>` | header or the tab-strip notice |
| "Release the folder", pending | `Sent. The station has not confirmed it yet.` (`role="status"`); the button reads `Releasing…`; the dialog stays open | release dialog |
| "Release the folder", pending, then refused | `Refused: <station message>` (the existing `ErrorNotice`); the dialog stays open | release dialog |
| "Release the folder", the follow stops | `The station did not confirm the release in time. Check the command card again.` | release dialog |
| A prompt with shell chips, pending | the chips stay above the composer (`Includes the output of !<command>`) until the station confirms the prompt | composer |
| A prompt with shell chips, pending, then refused (or the follow stops) | the refusal on the header line; the chips stay, and the next prompt carries them | header, composer |
| "Resume all" with resumes that are still pending | `Resumed <n> of <total>. <p> not confirmed yet` (`Resumed <n> of <total>` when none is pending, unchanged) | the ended tools row |

#### 4.2.5 The consumers of session commands (revisions 2 and 3)

Every consumer of a `SessionActions` command outcome on HEAD, and its decision. Revision 3 (design review r2 BLOCKER) widened the search, because revision 2 missed consumers that read `ok` in a callback. The search on HEAD is: every call of a `SessionActions` command in `web/src` outside the tests (`actions.send(`, `actions.answer(`, `actions.resume(`, … for each method of `SessionActions`), and every `.ok` read in `web/src` outside the tests and `gen/`. Each hit is followed to the command that produced the outcome, through props (`onRelease`, `onSend`) and wrappers (`handoverFlow`, `resumeThenSend`). A consumer marked "own observation" does not read `later`, because it already waits for the effect on the card or in the execution check and shows a failure when the effect does not come. A success side effect inside a call that a consumer gives to `controller.act` is a consumer of its own (the row "composer, shell chips").

| Consumer | Commands | Decision |
| --- | --- | --- |
| `session/controller.ts` `act`: the header, the composer, the dock, the turns panel (`panelAct`, `SessionView.tsx:332`), the handover (`:219-224`), the model and mode menus | most commands | Follows each pending command (§4.2.2). |
| `session/SessionView.tsx` `onSend`, the shell chips (`shellChips.consume`, `:612`; revision 3) | `send` (also through `resumeThenSend`) | Consumes the chips only on a confirmed success: `onConfirmed` (§4.2.1 item 2, §4.2.3 item 2). On HEAD a pending `ok` consumed them before the controller followed the prompt. |
| `chat/ShellCard.tsx` `ReleaseDialog` (`onRelease` = `actions.releaseShell`, `SessionView.tsx:507`; revision 3) | `releaseShell` | Waits for the final outcome; only a confirmed release closes the dialog; a refusal or a stopped follow stays in the dialog (§4.2.3 item 9). |
| `board/FleetBoard.tsx` `OfflineTools` "Resume all" (`:222-238`; revision 3) | `resume` | Counts a pending resume apart, not as resumed (§4.2.3 item 10). |
| `board/NeedsYouRail.tsx` | `answer` | Follows (§4.2.3 item 4). |
| `watch/WatchPage.tsx` | `answer` | Follows; a final refusal is the tile note (§4.2.3 item 5). |
| `session/ExecutionDialog.tsx` | `reconcileExecution` | Waits for the final outcome; only a confirmed mark shows "Marked as ended" (§4.2.3 item 6). |
| `terminal/handover.ts` `returnToBoard` → `handoverFlow.back`, `App.tsx` `returnTerminal` | `resume` | Passes the follow-up through; the controller and the tab-strip notice follow it (§4.2.3 items 7 and 8). |
| `views/hibernate.ts` `resumeThenSend` | `resume`, `send` | No change. A refused resume makes the sends refused; the outcome of the last send (with its `later`) goes to `controller.act('send')`, which follows it. |
| `terminal/handover.ts` `startHandover` (via `handoverFlow.open`) | `stop` | No change: own observation (it waits up to 60 s for the session to end on the card, then says "The session is still stopping."). |
| `compare/compare.ts` `keepOther` (`ComparePage.tsx:459`) | `stop` | No change: own observation (it waits up to 30 s for the end on the card, then says "The session did not end in 30 seconds."). |
| `session/ExecutionStop.tsx` `sendStop` | `stopExecution` | No change: own observation (it polls the execution check for `last_stop` of its own command id; no result gives the alert `outcome_unknown`). |
| `session/SessionView.tsx` `drive` (`:127-133`), `session/HoldNotice.tsx`, `handoverFlow.release` | not session commands: `sharing/people.ts` (drive requests) and `ShellActions.release` (the terminal hold) | No change (they do not use `sessionActions.run`). |
| `session/FleetControlChip.tsx`, `wall/DiffPane.tsx`, `wall/RacePane.tsx`, `wall/ReviewPane.tsx`, `wall/WallPage.tsx`, `session/paneLayout.ts`, `app/SessionRoute.tsx`, `compare/ComparePage.tsx` (other uses), `runtime/RuntimeApp.tsx` | none (readers: `files`, `fleetAnswers`, `canDrive`) or pass-through | No change. |

### 4.3 Out of scope

- The spawn sheet, pairing and the worktree actions keep their own outcome code (they do not use `sessionActions.run`).
- A board event for command state changes (the board WebSocket carries no commands); a code-specific text for `unknown_request` (the station text is shown as is, `commands.ts:1-6`).

## 5. Errors and edge cases

| Case | Result |
| --- | --- |
| G1: an outside start with an empty station text | `rejected: outside_roots` |
| G1: a station text of 2 000 characters with a newline | the reason has 300 characters of text, the newline is a space, then `…` |
| G1: the import of a native session outside the roots | `outside_roots` with the same text |
| G2: `station` names two stations with the same name | `invalid_params` "no paired station has this id or this unique name" |
| G2: `station` with view `me` | ignored |
| G2: a cursor of an unfiltered page used with a filter | valid; the page continues after the cursor key with the filter |
| G4: the deck answers `pending` and the station never answers | the follow reads for 120 s, then stops; the pending text stays |
| G4: the owner opens another session while the board follows | every follow of the old session is aborted; nothing is shown |
| G4: answer A is pending, then the owner answers B (pending too); the receipts come B first, then A, both refused | both follows continue; B's refusal shows under B, then A's under A; the header shows neither (the dock shows them) |
| G4: a command is pending, the owner does another action, then the first command is accepted | quiet: the newer outcome stays on the header line (a success after a newer outcome or a dismiss does not show) |
| G4: the owner dismisses the header line while a command is pending, then the station refuses it | the refusal shows |
| G4: `GET /api/commands/<id>` fails three times in a row | the follow stops; the pending text stays |
| G4: an e2e fake deck without the GET route (404) | three failed reads, then the follow stops |
| G4: a prompt with shell chips is pending, then refused | the header shows the refusal; the chips stay; the next prompt names them again |
| G4: a prompt with shell chips is pending; the owner sends a second prompt before the first is confirmed; both are accepted | both prompts name the chips; the station carries each result once (the first decided prompt takes it, `included_in`); the chips go when the first confirmed success comes |
| G4: a prompt with shell chips is pending and the owner opens another session | the follow is aborted; the chips of the first session stay (in memory, as today) |
| G4: a release of a shell folder is pending, then refused | the dialog stays open and shows the refusal; the card keeps "Release the folder" |
| G4: the owner closes the release dialog while the release is pending | the follow ends; the card shows the result when the station decides (`Released` or unchanged) |
| G4: "Resume all" over 3 sessions: one `Done.`, one pending, one refused | `Resumed 1 of 3. 1 not confirmed yet` |
| G5: a deck database at V48 with 10 000 rows | V49 keeps them as legacy tombstones (session `''`); the suppression state stays; the eviction rules apply to them as before |
| G5: a call of `wake_reserve` with an empty session | `Error` (never wakes); no row is written |

## 6. Risk class

**High.** G5 is a deck migration that rebuilds the table `wake_seen` with a new primary key (its rows are copied as legacy tombstones, §3.2 item 2, §3.3; no row is lost). The other gaps are normal: G1 adds a reject code to the receipt schema that the station, the deck, the MCP tools and the board read (additive, with the `Unknown` fallback); G2 changes the answer of the coordinator tool `fleet_view`; G4 is board code only. No authentication, authorization, money or personal data change. Review: the adversary DESIGN REVIEW on this document, then PRE-PR REVIEW of the final branch.

## 7. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Do not use `--release`, `--all-features` or a plain `cargo clean`.

The owned files of T1 to T4 do not overlap; any two can run in parallel (the machine allows two workers).

Cross-track notes:
- T1 starts after wall-control B1 (deck V48) is merged into `v2`: V49 is then the 49th entry of `migrations()`. If the architect starts T1 earlier, the brief gives the position 48 for the tests, and the architect renumbers at the merge. wall-control B1 also owns `schema.rs` and `tests/merged_migrations.rs`: do not run T1 in parallel with B1.
- last-seen T1 owns `crates/fleetdeck-deck/src/store/mod.rs`; no task here changes that file.
- last-seen T2 owns `web/src/board/fleet.test.tsx`; T4 puts its "Resume all" tests in a new file and does not change that file.

### T1 (backend): G5, the wake key with the session, migration V49 (≈ 340 lines)

- **Owned files:**
  - `crates/fleetdeck-deck/src/wake/scan.rs`
  - `crates/fleetdeck-deck/src/store/wake_seen.rs`
  - `crates/fleetdeck-deck/src/store/wake_seen_schema.rs` (new)
  - `crates/fleetdeck-deck/src/store/wake_seen_tests.rs` (new)
  - `crates/fleetdeck-deck/src/store/schema.rs` (one line)
  - `crates/fleetdeck-deck/src/store/display_gen_tests.rs`
  - `crates/fleetdeck-deck/tests/desktop_wake.rs` (the two `wake_reserve` calls only)
  - `crates/fleetdeck-deck/tests/desktop_wake_session_key.rs` (new)
  - `crates/fleetdeck-deck/tests/merged_migrations.rs`, `merged_migrations_v40.rs`, `merged_migrations_idle.rs`, `merged_migrations_bg3.rs`, `merged_migrations_p0.rs` (the latest deck version only)
- **Changes:** §3.2 items 2 to 5. `wake_seen.rs` also declares `#[cfg(test)] #[path = "wake_seen_tests.rs"] mod tests;` at its end. Existing test calls: `display_gen_tests.rs` uses session `"x"` in every `wake_reserve` call and `session_id` `'x'` in the raw `INSERT` at `:556`; `desktop_wake.rs:345` uses session `"s1"` (the request is pushed for `s1` at `:360`); `desktop_wake.rs:406` uses session `"seed-session"`. The five migration tests change the latest deck version from 47 to 49 (48 if the brief says so).
- **Tests:**
  - `wake_seen_schema.rs`, `#[cfg(test)] mod tests`: `v49_keeps_the_old_rows_as_legacy_tombstones_and_keys_by_session`: `migrations().to_version(&mut conn, 48)`; insert the V25 row `('s','r1',5)` and set `wake_state.suspended_until_ms = 7`; `to_version(49)`; `wake_seen` holds exactly one row, `('s','','r1',5)`; `suspended_until_ms` is 7; insert `('s','a','r1',1)` and `('s','b','r1',1)` succeed; a second `('s','a','r1',2)` fails; `wake_seen_first_seen` is in `sqlite_master`; no table `wake_seen_v49` is left.
  - `wake_seen_tests.rs` (`reserve` is called on a `rusqlite::Connection` migrated with `super::super::schema::migrations()`):
    - `one_request_id_in_two_sessions_is_two_wake_keys`: `(s, a, r1)` Reserved; `(s, b, r1)` Reserved; `(s, a, r1)` Seen; `(t, a, r1)` Reserved.
    - `the_session_key_survives_a_restart`: a file store (`tempfile`), `(s, a, r1)` Reserved; reopen with `DeckStore::open`; `(s, a, r1)` Seen; `(s, b, r1)` Reserved.
    - `a_v48_row_blocks_its_request_id_in_every_session_until_it_ages_out` (the upgrade regression of the review at the store level): `to_version(48)`; insert the V25 row `('s','r1',t0)` with `t0 = 1_000_000`; `to_latest()`; at `t0 + 1_000`: `(s, a, r1)` Seen, `(s, b, r1)` Seen, `(t, a, r1)` Reserved, `(s, a, r2)` Reserved; at `t0 + WAKE_SEEN_TTL_MS + 1` (the tombstone is deleted): `(s, c, r1)` Reserved and `(s, a, r1)` Seen (the key recorded under the tombstone stays).
    - `an_empty_session_is_never_a_wake_key`: `(s, "", r9)` → `Error`; `count(*) FROM wake_seen` is 0.
  - `tests/desktop_wake_session_key.rs` (helpers copied from `desktop_wake_cooldown.rs`: `n`, `now`, `requested`, `off`, `until`, `answer_info`; and from `desktop_wake.rs`: `start`, `advance`, `serve_wake`, `quiet`, `wake_rows`; a bound display as in `desktop_wake.rs` `bound()`; `bound_file(dir)` is the same with the body of `share_support::setup()` copied and `fixture_file(dir)` in place of `fixture()`):
    - `one_request_id_in_two_sessions_is_two_requests_for_the_wake`: start the task; push `requested(1, "s1", t, "r1")` and `requested(2, "s2", t, "r1")` in one `apply_events`; `until reserved == 2`; serve one `desktop.info` (off) and one `desktop.act` (ok); `until sent_ok == 1`. Then advance 31 s, push `requested(3, "s3", now, "r1")`; `reserved == 3`; serve the wake; `sent_ok == 2`. (On HEAD `reserved` stays 1: the regression.)
    - `a_reserved_key_of_one_session_does_not_block_another_session_after_a_restart`: before the task starts, `wake_reserve(station, "s1", "r1", t)` is Reserved (an earlier process); start the task; push `requested(1, "s2", now, "r1")`; serve the wake; `sent_ok == 1`. Advance 31 s; push `requested(2, "s1", now, "r1")`; `until seen == 1`; `no_request` for 300 ms; `sent_ok == 1`.
    - `a_legacy_tombstone_blocks_a_request_restored_after_an_empty_baseline` (the upgrade regression of the review at the scan level): `let dir = tempfile::tempdir()`; `bound_file(dir.path())`; `t = now(&f)`; a second `rusqlite::Connection` on `dir/deck.db` (`busy_timeout` 5 s) inserts `INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms) VALUES (?1, '', 'r1', ?2)` with `f.station` and `t` (the row that V49 makes of a V48 row; the station id is made by the fixture, so the V48 seed itself is in the store test above); start the task (its baseline sees no request: `baseline == 0`); `advance(&f, 2)`; `apply_snapshot` under a new incarnation `fake-station-db-2` with `card("s1")` (no request), then `apply_events` under it with `requested(1, "s1", now, "r1")`; `until seen == 1`; `quiet`; `sent_ok + reserved == 0`; `wake_rows` is empty. Then `apply_events` under the same incarnation with `requested(2, "s1", now, "r2")`; serve the wake; `until sent_ok == 1`. (With the rev 1 migration, which dropped the rows, `r1` wakes: the regression.)
- **depends_on:** none in this document (see the cross-track note).
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(wake) | test(v49) | test(the_migrations_are_valid) | binary(desktop_wake) | binary(desktop_wake_cooldown) | binary(desktop_wake_session_key) | binary(merged_migrations) | binary(merged_migrations_v40) | binary(merged_migrations_idle) | binary(merged_migrations_bg3) | binary(merged_migrations_p0)'`
  - `cargo clippy -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards: `cargo nextest run -p fleetdeck-cli --no-fail-fast -E 'binary(module_size) | binary(layering)'`
  - `grep -n "wake_reserve(" crates/fleetdeck-deck/src crates/fleetdeck-deck/tests -r` shows four arguments in every call.

### T2 (backend): G1, the reject code `outside_roots` and the station text in the outbox reason (≈ 200 lines)

- **Owned files:**
  - `crates/fleetdeck-proto/src/command.rs`, `crates/fleetdeck-proto/src/command_tests.rs`
  - `web/src/gen/command/RejectCode.ts` (generated only, by `make ts-types`)
  - `crates/fleetdeck-station/src/manager/policy_limits.rs`, `crates/fleetdeck-station/src/manager/decide.rs` (the `Outside` arm only)
  - `crates/fleetdeck-station/src/manager/tests.rs` (`:419`, `:435` and two lines for the new module), `crates/fleetdeck-station/src/manager/import_tests.rs` (`:152`)
  - `crates/fleetdeck-station/src/manager/outside_roots_tests.rs` (new; declared in `manager/tests.rs` as `#[path = "outside_roots_tests.rs"] mod outside_roots;`, next to `request_key`)
  - `crates/fleetdeck-deck/src/store/outbox.rs`, `crates/fleetdeck-deck/src/store/outbox_tests.rs`, `crates/fleetdeck-deck/src/store/outbox_hold_tests.rs`
  - `crates/fleetdeck-deck/tests/outbox.rs` (`:134`), `crates/fleetdeck-deck/tests/board_fleet_control.rs` (one new test at the end)
- **Changes:** §1.2 items 1 to 4. Expected strings in the existing tests: `outbox_tests.rs:50-51` → `"rejected: already_answered: no"`, `"rejected: answer_cap_reached: no"`; `outbox_hold_tests.rs:184`, `:195`, `:221`, `:378` → `"rejected: invalid_state: refused"`, `:206` → `"rejected: forbidden: refused"`; `tests/outbox.rs:134` → `"rejected: unknown_session: no"`; `manager/tests.rs:419` and `:435` → `RejectCode::OutsideRoots` (`:441`, the `max_sessions` case, stays `Forbidden`); `import_tests.rs:152` → `RejectCode::OutsideRoots`.
- **Tests:**
  - `command_tests.rs`: `outside_roots_is_a_stored_reject_code`: `serde_json::to_value(RejectCode::OutsideRoots) == "outside_roots"`, it parses back, and `stores_receipt()` is true.
  - `outside_roots_tests.rs`:
    - `the_outside_roots_text_names_the_station`: `outside_roots_text("/x", "box") == "The folder /x is outside the allowed roots of station box."`; `outside_roots_text("/x", "") == "The folder /x is outside the allowed roots of this station."`.
    - `a_start_outside_the_allowed_roots_is_refused_with_outside_roots_and_its_text`: `Station::new(1)`, a start in `root/elsewhere`; the receipt is rejected with `OutsideRoots` and the message `outside_roots_text(<that cwd>, "")`.
  - `outbox_tests.rs`: `a_rejected_reason_carries_the_station_text_cut_and_clean`: message `""` → `rejected: forbidden`; message `"a\nb"` → `rejected: forbidden: a b`; a 400-character message → text of 300 characters plus `…`; no `reject` → `rejected: unknown`.
  - `board_fleet_control.rs`: `the_status_of_a_refused_call_names_the_code_and_the_station_text`: as the existing status test (`:552-626`) with a `Drive` grant and `call(…, "k9", stop(f, "w1"))`; the fake station answers `Receipt::rejected(id, Reject::new(RejectCode::OutsideRoots, "The folder /tmp/x is outside the allowed roots of station box."), 0)`; wait until `deliveries[0].state == "dead"`; `deliveries[0].reason == "rejected: outside_roots: The folder /tmp/x is outside the allowed roots of station box."`.
- **depends_on:** none.
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-proto --no-fail-fast -E 'test(outside_roots) | binary(ts_drift)'`
  - `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(outside_roots) | test(the_station_policy_decides_in_the_command_transaction) | test(import)'`
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(outbox) | test(rejected_reason) | binary(outbox) | binary(board_fleet_control)'`
  - `cargo clippy -p fleetdeck-proto -p fleetdeck-station -p fleetdeck-deck --all-targets -- -D warnings`
  - The guards (as T1).

### T3 (backend): G2, the station filter of `fleet_view` (≈ 220 lines)

- **Owned files:**
  - `crates/fleetdeck-deck/src/store/fleet_view.rs`, `crates/fleetdeck-deck/src/store/fleet_view_build.rs`
  - `crates/fleetdeck-deck/src/store/fleet_view_filter_tests.rs` (new; declared at the end of `fleet_view.rs` as `#[cfg(test)] #[path = "fleet_view_filter_tests.rs"] mod filter_tests;`; it uses the helpers of `super::super::fleet_view_tests`)
  - `crates/fleetdeck-proto/src/fleet_view.rs` (the doc of `FleetViewRequest` only)
  - `crates/fleetdeck-station/src/coordinator/tools.rs` (the description of `fleet_view` only)
- **Changes:** §2.2 items 2 to 7.
- **Tests** (`fleet_view_filter_tests.rs`; stations `alpha` and `beta` with sessions; the caller is a session of `alpha`):
  - `the_station_filter_keeps_the_sessions_of_one_station_by_id_or_name`: 3 sessions on `alpha`, 2 on `beta`; filter by the id of `beta`, by `"beta"` and by `"BETA"`: each gives the 2 sessions of `beta` only.
  - `the_station_filter_keeps_one_station_in_the_stations_view`: filter `"alpha"` gives one item with that `station_id`.
  - `the_station_filter_of_the_projects_view_takes_a_name`: one project on each station; filter `"beta"` gives the project of `beta` only.
  - `an_unknown_or_ambiguous_station_filter_is_refused`: `"nope"` → `refusal == UNKNOWN_STATION`; two more stations named `"dup"`, filter `"dup"` → `UNKNOWN_STATION`.
  - `the_me_view_ignores_the_station_filter`: view `me` with `"nope"` answers the caller.
  - `paging_with_the_station_filter_lists_every_session_of_the_station`: 130 sessions on `alpha`, 20 on `beta`; page through `sessions` with filter `"alpha"` (at most 10 pages, a fresh caller per page as in `fleet_view_paging_tests.rs:58-80`): 130 unique ids, none of `beta`, and the first page has a `next_cursor`.
- **depends_on:** none.
- **Acceptance:**
  - `cargo nextest run -p fleetdeck-deck --no-fail-fast -E 'test(fleet_view) | test(station_filter) | test(paging)'`
  - `cargo nextest run -p fleetdeck-station --no-fail-fast -E 'test(tools) | test(fleet_view) | test(mcp_p3)'`
  - `cargo clippy -p fleetdeck-deck -p fleetdeck-proto -p fleetdeck-station --all-targets -- -D warnings`
  - The guards (as T1).

### T4 (ux): G4, the board follows a pending command and shows a refused answer where the owner clicked (≈ 620 lines)

- **Owned files:**
  - `web/src/session/commands.ts`, `web/src/session/controller.ts`
  - `web/src/session/ApprovalDock.tsx`, `web/src/session/SessionView.tsx`, `web/src/session/SessionHeader.tsx`
  - `web/src/board/NeedsYouRail.tsx`, `web/src/watch/WatchPage.tsx`
  - `web/src/session/ExecutionDialog.tsx`, `web/src/terminal/handover.ts`, `web/src/App.tsx` (`returnTerminal` only)
  - `web/src/chat/ShellCard.tsx` (`ReleaseDialog` and the two new exports only), `web/src/board/FleetBoard.tsx` (`resumeEach`, `resumeReport` and `resumeAll` only) (revision 3)
  - `web/src/session/follow.test.ts` (new), `web/src/session/shown.test.ts` (new), `web/src/session/approvalDock.test.tsx`, `web/src/board/rail.test.tsx`, `web/src/session/execution.test.tsx`, `web/src/terminal/terminal.test.tsx`
  - `web/src/session/sessionheader_access.test.tsx`, `web/src/session/controls.test.tsx`, `web/src/session/sessionMenu.test.tsx` (each: add `refusals: {},` after `outcome: null,` in the `Controller` literal; `execution.test.tsx:169` the same)
  - `web/src/chat/shell.test.tsx`, `web/src/board/resumeAll.test.ts` (new; `web/src/board/fleet.test.tsx` is an owned file of last-seen T2, so this task does not change it) (revision 3)
  - `web/e2e/session.spec.ts`, `web/e2e/shell-bang.spec.ts` (revision 3: new tests at the end and one copied helper)
- **Changes:** §4.2 (4.2.1 to 4.2.3, with the revision 3 items: `onConfirmed` in 4.2.1 item 2, the chips in 4.2.3 item 2, 4.2.3 items 9 and 10; 4.2.5 is the audit: the consumers marked "No change" are not changed). Imports: `controller.ts` takes `OpenRequest` from `../gen` and `Reply` from `./question`; `NeedsYouRail.tsx` takes `nextShown`, `shownText` and `type Shown` from `../session/controller` and `type Outcome` from `../session/commands`; `ExecutionDialog.tsx` takes `finalOutcome` from `./commands`; `ShellCard.tsx` takes `finalOutcome` and `type Outcome` from `../session/commands`; `FleetBoard.tsx` uses its existing `SessionActions` import. Load the `fleet-design-system` skill first (board UI rule); the design uses only the existing `.fd-q-error` and `.fd-note` lines. `SessionView.tsx` must stay at or below 797 lines (the design gives 794); `App.tsx` must stay below 800 lines. In `web/e2e/*.spec.ts`, if a test fails only because a follow `GET /api/commands/<id>` is in `deck.calls`, add `&& c.body !== null` to its filter; change no other e2e line outside `session.spec.ts` and `shell-bang.spec.ts`.
- **Tests:**
  - `follow.test.ts` (injected `wait` that records the ms and resolves at once, and an injected `now`):
    - `a pending command is read until the station refuses it`: GET answers pending, pending, done with a rejected receipt (message `no such request`) → `{ ok: false, text: 'Refused: no such request' }`; three GETs of `/api/commands/c1`; waits `[1000, 1000, 1000]`.
    - `the reads slow down after ten`: 11 pending answers, then done → the 11th wait is 5000.
    - `an abort stops the follow`: abort after the first read → `null`, no second GET.
    - `three failed reads in a row stop the follow; a good read resets the count`: throw, throw, done → `Done.`; throw ×3 → `null`.
    - `the follow stops after 120 s`: `now` moves 121 s after the first wait → `null`.
    - `sessionActions follows only a pending answer and only with a reader`: a pending reply with `get` → `later` is a function; with `get = null` → no `later`; a done reply → no `later`.
    - `finalOutcome gives a final outcome as is and waits for a provisional one`: `{ ok: true, text: 'Done.' }` → the same object; an outcome whose `later` resolves `{ ok: false, text: 'Refused: x' }` → that outcome; a `later` that resolves `null` → `null`.
    - `onConfirmed runs its action only on a confirmed success` (revision 3): a final `D` → `done` ran once and the result is the same object; a final `R` → `done` did not run; `P` whose `later` resolves `D` → `done` has not run when `onConfirmed` returns, and ran once after `await result.later(signal)`, which gives `D`; `P` → `R`: `done` did not run and `later` gives `R`; `P` → `null`: `done` did not run and `later` gives `null`; `P` → `D` with a signal aborted before `later` resolves: `done` did not run.
  - `shown.test.ts` (the pure state functions of `controller.ts`; `P = { ok: true, text: 'Sent. The station has not confirmed it yet.', later: async () => null }`, `R = { ok: false, text: 'Refused: no such request' }`, `D = { ok: true, text: 'Done.' }`):
    - `nextShown counts a repeat of the same refusal`; a different text, target or action resets to 1; `shownText` adds ` (2 times)`.
    - `an immediate refusal repeated counts in the dock`: `onFirst(EMPTY_SHOWN, 'answer', 'r1', R)` twice → `refusals.r1.repeats === 2`; `dockRefusals(state.refusals, [open r1])` → `{ r1: 'Refused: no such request (2 times)' }`.
    - `a pending, then refused answer repeated counts` (review MINOR): `s1 = onFirst(EMPTY, 'answer', 'r1', P)`; `s2 = onFinal(s1, s1.outcome, 'answer', 'r1', R)`; `s3 = onFirst(s2, 'answer', 'r1', P)`; `s4 = onFinal(s3, s3.outcome, 'answer', 'r1', R)` → `s4.refusals.r1.repeats === 2` and `s4.settled.repeats === 2`; `s3.refusals.r1.repeats === 1` (a pending outcome does not touch the refusals).
    - `two pending answers whose receipts come in reverse order both show their refusal` (review MAJOR 1): `a = onFirst(EMPTY, 'answer', 'rA', P)`; `b = onFirst(a, 'answer', 'rB', P)`; `c = onFinal(b, b.outcome, 'answer', 'rB', R)`; `d = onFinal(c, a.outcome, 'answer', 'rA', R)` → `d.refusals` has the keys `rA` and `rB` (both refusals with `repeats === 1`); `dockRefusals(d.refusals, [open rA, open rB])` has both; `headerOutcome({ outcome: d.outcome, refusals: d.refusals }, [open rA, open rB])` is `null`.
    - `a success after a newer outcome stays quiet; a refusal shows`: `a = onFirst(EMPTY, 'stop', null, P)`; `b = onFirst(a, 'mode', null, R)`; `onFinal(b, a.outcome, 'stop', null, D).outcome` is `b.outcome`; `onFinal(b, a.outcome, 'stop', null, R)` → `outcome.action === 'stop'`; `onFinal(a, a.outcome, 'stop', null, D).outcome.text === 'Done.'`.
    - `an accepted answer clears its refusal`: an immediate `R` for `r1`, then `onFirst(…, 'answer', 'r1', D)` → `refusals.r1` is undefined.
    - `headerOutcome hides only a refused answer that the dock shows`: an accepted answer → itself; action `send` refused → itself; a refused answer whose request is not open → itself; with `refusals: {}` → itself.
  - `approvalDock.test.tsx`: `a refusal is shown under its request only`: two requests, `refusals={{ r2: 'Refused: no such request (2 times)' }}` → one `role="alert"` line, inside the `data-request="r2"` item.
  - `rail.test.tsx`:
    - `the rail keys a request by station, session and request id`: `railKey({ station_id: 'a', session_id: 's1' }, 'req-3') !== railKey({ station_id: 'a', session_id: 's2' }, 'req-3')`.
    - `the rail counts a delayed repeat of a refusal` (review MINOR): `e0 = {}`; `railErrors(e0, 'k', P)` is `e0`; `e1 = railErrors(e0, 'k', R)` → `e1.k.repeats === 1`; `railErrors(e1, 'k', P)` is `e1`; `railErrors(e1, 'k', R).k.repeats === 2`.
  - `execution.test.tsx` (review MAJOR 2):
    - `a pending mark is followed until the station refuses it`: `reconcileExecution` answers `{ ok: true, text: 'Sent. The station has not confirmed it yet.', later: async () => ({ ok: false, text: 'Refused: a process of the run still lives' }) }` → `markEnded` gives `{ marked: false, text: 'Refused: a process of the run still lives' }`; `onPending` was called once with the pending text.
    - `only a confirmed mark marks`: `Done.` at once → `{ marked: true }` and no `onPending`; pending then `Done.` → `{ marked: true }`; pending then `null` → `{ marked: false, text: MARK_UNCONFIRMED }`.
  - `terminal.test.tsx` (`describe('returnToBoard')`): `a pending resume keeps its follow-up`: as the first test of the block, with `resume` answering `{ ok: true, text: 'Sent. The station has not confirmed it yet.', later: async () => ({ ok: false, text: 'Refused: x' }) }` → `result.text === 'Back under board control. The station has not confirmed the resume yet.'`; `await result.later(new AbortController().signal)` → `{ ok: false, text: 'Returned, but the resume failed: Refused: x' }`.
  - `shell.test.tsx` (`describe('the shell card')`, revision 3, design review r2 BLOCKER; `P` and `D` as in `shown.test.ts`):
    - `a pending release is followed until the station refuses it`: `releaseFollowed(async () => ({ ...P, later: async () => ({ ok: false, text: 'Refused: the command still runs' }) }), new AbortController().signal, onPending)` → `{ ok: false, text: 'Refused: the command still runs' }`; `onPending` was called once with `'Sent. The station has not confirmed it yet.'`.
    - `only a confirmed release closes the dialog`: `D` at once → `{ ok: true, text: 'Done.' }` and no `onPending`; `P` then `D` → `{ ok: true, text: 'Done.' }`; `P` then `null` → `{ ok: false, text: RELEASE_UNCONFIRMED }`.
  - `board/resumeAll.test.ts` (new, revision 3):
    - `resume all counts only a confirmed resume`: `resumeEach` over five targets: `resume` gives `D`; `resume` gives `P`; `resume` gives `{ ok: false, text: 'Refused: x' }`; `null`; `resume` throws → `{ confirmed: 1, pending: 1 }`.
    - `the report names the pending resumes apart`: `resumeReport(2, 0, 3) === 'Resumed 2 of 3'`; `resumeReport(1, 2, 3) === 'Resumed 1 of 3. 2 not confirmed yet'`.
  - `web/e2e/session.spec.ts` (new tests at the end, with the file's helpers):
    - `a refused answer in the session dock shows its text under the request, and a repeat counts`: `approval.answer` answers `receipt(id, 'done', 'no such request')`; open the session; click Allow in the region "Needs you"; the dock shows the alert `Refused: no such request`; click Allow again; the alert reads `Refused: no such request (2 times)`; the session header has no alert with that text.
    - `a pending answer is followed until the station refuses it`: `approval.answer` answers `receipt(id, 'pending')`; `deck.route('GET', /^\/api\/commands\/([^/]+)$/, (m) => ({ status: 200, body: receipt(m[1], 'done', 'no such request') }))`; click Allow; the dock alert `Refused: no such request` appears within 5 s.
  - `web/e2e/shell-bang.spec.ts` (new tests at the end, revision 3, design review r2 BLOCKER; copy the `receipt` helper of `session.spec.ts:45-…` with `ST`; the GET route is `deck.route('GET', /^\/api\/commands\/([^/]+)$/, (m) => ({ status: 200, body: receipt(m[1], 'done', '<text>') }))`):
    - `a pending prompt that the station refuses keeps its shell chip`: `setUp(page)`, then `deck.on('/api/commands/session.prompt', (b) => ({ status: 200, body: receipt(idOf(b), 'pending') }))` and the GET route with `'the session is busy'`; emit `shellEvents('pwd', '/home/me/app\n')`; the chip `Includes the output of` is visible; send `what did it print?`; the session header shows the alert `Refused: the session is busy` within 5 s; the chip is still visible. Then `deck.on('/api/commands/session.prompt', accepted('session.prompt'))`; send `again`; the chip has count 0; the last prompt body has `shell_results: ['e1']`.
    - `a pending release that the station refuses stays in the dialog`: the ended session of `on an ended session the owner releases the folder after a question`; `session.release_shell` answers `receipt(idOf(b), 'pending')`; the GET route with `'the command still runs'`; click "Release the folder", then Release; the dialog shows `Sent. The station has not confirmed it yet.`, then `Refused: the command still runs` within 5 s; the dialog is still open (count 1); Cancel closes it.
- **depends_on:** none (no generated type changes; it does not read the new reject code).
- **Acceptance:**
  - `cd web && bun run typecheck && bun run lint`
  - `cd web && bun run test src/session src/board src/watch src/terminal src/chat` (through heavy.sh)
  - `cd web && bun run e2e session.spec.ts revert.spec.ts shell-bang.spec.ts` (through heavy.sh)
  - `sh tools/design-check.sh`
  - The guards (as T1).
  - `wc -l web/src/session/SessionView.tsx web/src/App.tsx`: at most 797 and below 800.
  - `grep -n 'shellChips.consume' web/src/session/SessionView.tsx` shows one line, inside `onConfirmed(`; `grep -n 'if (outcome.ok) onClose' web/src/chat/ShellCard.tsx` still shows one line, after `releaseFollowed` (revision 3).
  - Gallery shot of the session view with an open approval, both themes, desktop and phone (the fleet-design-system skill §3 step 3): the refusal line under the request.

## 8. Decisions made in this draft

1. **G1: a new reject code `outside_roots`**, not `forbidden` with a better text. A caller can act on a code (pick another folder) and tell it from a grant refusal. The word matches the existing link error code `outside_roots`. Rejected: `cwd_outside_roots` (a second word for one meaning); a link feature flag (the receipt schema has an `Unknown` fallback).
2. **G1: the refusal text names the folder that the caller sent and the station name**, not the canonical path and not the allowed roots. Rejected: listing the roots (it shows the owner's folder layout to every caller; the owner sets the roots on the board).
3. **G1: the outbox reason carries the station text** (`rejected: <code>: <text>`, at most 300 characters, control characters as spaces). One change fixes `command_status`, the task rows and the mail rows. Rejected: a new `reject` object in `command_status` (a second place for the same fact; the task and mail rows would stay without the text).
4. **G1: no check on the deck.** The deck does not know the roots and cannot resolve symlinks on the station. Rejected: send the roots in `StationInfo` (a new field in every link handshake for one error text).
5. **G2: the filter applies to `stations`, `projects` and `sessions`**; `me` ignores it. Rejected: sessions only (the schema already says "Station id or name." for every view).
6. **G2: a filter that names no station, or an ambiguous name, is `invalid_params`.** Rejected: an empty page (a typo looks like an empty fleet).
7. **G5: the old `wake_seen` rows are kept as legacy tombstones** in V49 (session `''`). A tombstone makes its `(station, request_id)` `Seen` in every session until it ages out (at most 24 hours), so a request that an earlier deck reserved never wakes again, also when the station restores it after the startup baseline (revision 2, design review r1 BLOCKER). Cost: for at most 24 hours after the upgrade, a new session whose provider id repeats an id of the tombstones does not wake a wall (the behavior of HEAD, fail closed). Rejected: drop the rows (revision 1; the baseline does not see a request that is restored later, so the replay protection was lost); a tombstone that blocks for 120 s only (`WAKE_SEEN_PROTECT_MS`; a restored request can carry a fresh `requested_at`, as in `tests/desktop_wake.rs:358-362`, so the age window does not protect it); derive the session from the cards (the cards of the moment do not hold every old request, and exactly the absent ones are the risk).
8. **G5: the migration file is declared from `wake_seen.rs`, and the list entry is one line.** `store/mod.rs` (796) and `schema.rs` (795) are near the 800-line limit.
9. **G4: the report's cause is corrected** (§4.1): the board gets the receipt in the HTTP answer. The fix places the refusal where the owner clicked, counts a repeat, and follows a command that is still pending after 5 s.
10. **G4: the board reads `GET /api/commands/<id>`** (1 s ×10, then 5 s, at most 120 s, 3 failed reads stop it). Rejected: a new board WebSocket event for commands (a deck and protocol change for a rare case); a longer `RECEIPT_WAIT` (it holds an HTTP request and the button for longer).
11. **G4: the dock shows a refused answer under its request**, and the header does not show it twice. Rejected: a toast (no toast primitive in the design system); the header only (the case that the owner missed).
12. **G4: a repeat is counted in the text** (`(2 times)`). Rejected: an animation (motion rules of the design system; a screen reader does not see it).
13. **G4: the rail keys its state by station, session and request id**, the DL-402 rule on the board.
14. **G4: a success after a dismiss or a newer outcome stays quiet; a refusal always shows.**
15. **G4: each pending command has its own follow until it ends or the session view ends** (revision 2, design review r1 MAJOR 1). A newer action does not abort an older follow. Rejected: one follow for each controller (revision 1; a second action dropped the refusal of the first).
16. **G4: the controller keeps the refused answers by request** (`refusals`), and the dock shows each one under its request. The header line stays one outcome. Rejected: one shown outcome for the dock too (two refusals for two requests would hide one of them).
17. **G4: the repeat count compares a final outcome with the previous final outcome** (`settled`, and the refusal of the same request); a pending text is not counted and does not reset the count (revision 2, design review r1 MINOR).
18. **G4: an outcome with `later` is provisional; every consumer is audited** (§4.2.5, revision 2, design review r1 MAJOR 2). "Mark as ended" waits for the final outcome; "Return" passes the follow-up through; the consumers that already watch the effect on the card or in the execution check do not change. Rejected: make `run` wait for the final outcome for every command (it would hold every button for up to 120 s); a `pending` flag in `Outcome` (a second marker for the same fact as `later`).
19. **G4: a success side effect runs only on a confirmed success** (revision 3, design review r2 BLOCKER). The release dialog closes only on a confirmed release (`releaseFollowed`, `RELEASE_UNCONFIRMED`); the shell chips are used up only when the prompt is confirmed (`onConfirmed`); "Resume all" counts a pending resume apart (`resumeEach`, `resumeReport`). The audit (§4.2.5) now follows every `.ok` read through props and wrappers. Rejected: hide the chips while the prompt is pending and show them again on a refusal (a session change returns before the follow starts, so the chips would stay hidden; and a second prompt that names them is safe, because the station carries each result once); make "Resume all" follow each pending resume (it would hold the button for up to 120 s per session; each card shows its state).

## 9. Owner questions

1. **Should the board list the failed calls of a coordinator (for example a spawn refused outside the roots) on the coordinator's session?** Today the owner sees them only in the coordinator's chat (the `command_status` result). Recommended default: not in this track; the new reason text reaches the chat and the task and mail rows.
2. **Is "(2 times)" good text for a repeated refusal?** Recommended default: yes.

## Decision log entries (to append)

## DL-510: G1 a start outside the allowed roots is refused with `outside_roots` and a text that names the folder and the station (design agent, 2026-10-23)

- Context: G1; design `docs/v2/design/fleet-gaps.md` §1. A coordinator spawn into a folder outside the roots ended as `rejected: forbidden` with no reason (`manager/decide.rs:481`, `store/outbox.rs:281`).
- Decision: new `RejectCode::OutsideRoots` (`outside_roots`, stored) for `session.start` and `session.import`; text "The folder <cwd> is outside the allowed roots of station <name>." (`this station` for an empty name). No link feature flag: an older peer reads `unknown` and keeps the text.
- Rejected: `forbidden` with a new text; `cwd_outside_roots`; listing the roots in the text; a deck-side check.

## DL-511: G1 the outbox reason of a refused command carries the station text (design agent, 2026-10-23)

- Decision: `next()` writes `rejected: <code>: <text>` (text trimmed, control characters as spaces, at most 300 characters plus `…`); `rejected: <code>` for an empty text. `command_status`, the task rows and the mail rows show it without other changes.
- Rejected: a separate `reject` object in `command_status`.

## DL-512: G2 the `fleet_view` station filter works for stations, projects and sessions (design agent, 2026-10-23)

- Decision: the deck applies `station` (an id, or a unique name, also case-insensitive) to the `stations`, `projects` and `sessions` views; `me` ignores it. A filter that names no station, or an ambiguous name, is `invalid_params` "no paired station has this id or this unique name". The tool description says so.
- Rejected: sessions only; an empty page for an unknown station.

## DL-513: G4 the board follows a pending command with `GET /api/commands/<id>` (design agent, 2026-10-23)

- Context: the board gets the receipt in the HTTP answer when the station decides within 5 s (`command_routes.rs:85-90`; live: 76 refused answers settled in 14 ms on average). After a `pending` answer it never read the command again.
- Decision: `sessionActions.run` gives a `pending` outcome a `later` follow-up that reads `GET /api/commands/<id>` (1 s ×10, then 5 s, at most 120 s; three failed reads stop it). An outcome with `later` is provisional. The session controller follows each pending command on its own; the rail and the watch tiles show the final refusal; "Mark as ended" marks only on a confirmed outcome; "Return" keeps the follow-up of its resume; the shell release dialog closes only on a confirmed release; the shell chips are used up only by a confirmed prompt; "Resume all" counts a pending resume apart (revision 3). The consumers that watch the effect themselves (stop and handover, keep-one, the execution stop) do not change.
- Rejected: a WebSocket event for commands; a longer `RECEIPT_WAIT`; one follow for each controller (a newer action dropped the older refusal); a `run` that waits for the final outcome; chips hidden while pending and shown again on a refusal.

## DL-514: G4 a refused answer is shown under its request in the dock, and a repeat is counted (design agent, 2026-10-23)

- Decision: the session controller keeps the refused answers by request; the approval dock shows each one under its request with `.fd-q-error`; the session header does not repeat it. The same refusal again reads "… (2 times)", also when each attempt was pending first (the count compares final outcomes only). The rail keys its pending and error state by station, session and request id.
- Rejected: a toast; the header only; an animation.

## DL-515: G5 the wall-wake key has the session; deck migration V49 keeps the old `wake_seen` rows as tombstones (design agent, 2026-10-23)

- Context: request ids are per session (DL-402, station V43). The deck wake scan and `wake_seen` keyed requests by `(station, request_id)` (`wake/scan.rs:149`, `display_v25.rs:43-49`), so the second session with the same id never woke a wall.
- Decision: the key is `(station_id, session_id, request_id)` in `Scanner.known`, `present`, `wake_reserve` and the table. V49 rebuilds `wake_seen` with the new primary key and copies every old row as a legacy tombstone (session `''`); a tombstone makes its `(station, request_id)` `Seen` in every session until it ages out (24 hours). `wake_state` stays. An empty session in `wake_reserve` is `Error`.
- Rejected: drop the old rows (a request restored after the startup baseline would wake again; design review r1); a 120 s tombstone; derive the session of old rows from the cards.

## Revision 2 (design review round 1)

Report: `~/fleetdeck-briefs/backlog/gaps/adv-design-r1.md` (reviewed commit `8ddfebe2`, verdict FAIL). Each finding was checked against the code on `v2` before the change.

1. **BLOCKER, `wake_seen` replay protection (§3).** Confirmed: `wake/scan.rs:130-180` makes a baseline only on the first scan or a changed snapshot mark, so a request that is restored after the baseline (a new incarnation whose snapshot does not have it, then an event, as in `tests/desktop_wake.rs:339-369`) is not in the baseline; with the V48 rows dropped, it gave `Reserved` and could wake. The restored event can carry a fresh `requested_at`, so the 60 s window does not protect it. **Fixed:** V49 copies every V48 row as a legacy tombstone (session `''`); `reserve` answers `Seen` when a tombstone of `(station, request_id)` exists and still records the new key; an empty session is `Error`. Changed: §3.2 items 2 and 4, §3.3, §5, §6, T1 (tests `v49_keeps_the_old_rows_as_legacy_tombstones_and_keys_by_session`, `a_v48_row_blocks_its_request_id_in_every_session_until_it_ages_out`, `an_empty_session_is_never_a_wake_key`, `a_legacy_tombstone_blocks_a_request_restored_after_an_empty_baseline`), §8 item 7, DL-515. The scan-level test inserts the tombstone row directly, because the fixture makes the station id after the store opens; the store-level test seeds V48 and migrates. Cost, accepted: for at most 24 hours after the upgrade, a provider id that has a tombstone does not wake a wall in a new session (the behavior of HEAD).
2. **MAJOR, a newer action abandoned an older follow (§4.2.2).** Confirmed in the revision 1 pseudocode (`follow.current?.abort()` in every `act`). **Fixed:** each pending command has its own `AbortController` in a set; only a session change or the end of the view aborts them. The refused answers are kept by request (`refusals`) for the dock, so two refusals for two requests both show. The state changes are pure functions (`onFirst`, `onFinal`), tested in `shown.test.ts` with two pending answers whose receipts come in reverse order. Changed: §4.2.2, §4.2.3 items 1 to 3, §4.2.4, §5, T4, §8 items 14 to 16, DL-513, DL-514.
3. **MAJOR, consumers that do not read `later` (§4.2.1).** Confirmed: `ExecutionDialog.tsx:111-115` sets "Marked as ended" on `ok`, and a pending outcome is `ok: true`; `terminal/handover.ts:108-111` turns a pending resume into "Back under board control." and drops the follow-up. **Fixed:** the contract says that an outcome with `later` is provisional; §4.2.5 audits every consumer. "Mark as ended" now waits for the final outcome (`markEnded`, `MARK_UNCONFIRMED`); `returnToBoard` keeps the follow-up, and `App.tsx` `returnTerminal` shows a final refusal. The consumers that watch the effect themselves (`startHandover`, `keepOther`, `sendStop`, `resumeThenSend`) do not change, with the reason in §4.2.5. Changed: §4.2.1, §4.2.3 items 6 to 8, §4.2.4, §4.2.5, T4 (owned files, tests in `execution.test.tsx` and `terminal.test.tsx`), §8 item 18, DL-513.
4. **MINOR, a delayed repeat was not counted (§4.2.2).** Confirmed: revision 1 counted a final outcome against `null`. Fixed although it is minor, because the controller rewrite for finding 2 makes the fix free: the count compares a final outcome with the last final outcome (`settled`) and with the last refusal of the same request; a pending outcome does not touch either. The rail uses `railErrors` with the same rule. Tests: `a pending, then refused answer repeated counts` (`shown.test.ts`) and `the rail counts a delayed repeat of a refusal` (`rail.test.tsx`). Changed: §4.2.2, §4.2.3 item 4, T4, §8 item 17.

Not fixed: none. The task ids do not change; T1 and T4 grow (≈ 340 and ≈ 500 lines) and keep their owned files disjoint from T2 and T3.

## Revision 3 (design review round 2)

Report: `~/fleetdeck-briefs/backlog/gaps/adv-design-r2.md` (reviewed commit `b612bfee`, verdict FAIL). The report confirms the r1 BLOCKER (V49 replay protection), the r1 MAJOR (abandoned follow-up) and the r1 MINOR (repeat count) as fixed. It has one finding. It was checked against the code on `v2` (`00055138`) before the change.

1. **BLOCKER, the consumer audit misses actions that treat pending as confirmed (§4.2.5, confidence 0.99).** Confirmed for both paths:
   - `SessionView.tsx:507` gives `actions.releaseShell` to the shell card as `onRelease`; `ReleaseDialog.release` (`chat/ShellCard.tsx:107-115`) calls `onClose()` on any `ok`, so a pending release closes the dialog, and a later refusal is shown nowhere.
   - `SessionView.tsx:612` calls `shellChips.consume(results)` on any `ok` inside the call that it gives to `controller.act('send')`, so a pending prompt hides its chips before the controller follows it; a later refusal leaves them hidden, and the next prompt does not name them (`useShellChips`, `chat/ShellChips.tsx:46-63`).

   **Fixed:**
   - The contract (§4.2.1 item 1) now names a success side effect inside a call given to `controller.act`.
   - New helper `onConfirmed` (§4.2.1 item 2): the side effect runs on a final `ok` at once, or when the follow-up resolves `ok`; never on a refusal, a stopped follow or an aborted follow.
   - The composer uses up the chips through `onConfirmed` (§4.2.3 item 2; `SessionView.tsx` goes to 794 lines). A second prompt that names the same chips while the first is pending is safe: the station carries each result once (`included_in`, `store/shell_runs.rs:379-388`).
   - `ReleaseDialog` follows a pending release (`releaseFollowed`, `RELEASE_UNCONFIRMED`, §4.2.3 item 9): the dialog shows the pending note, closes only on a confirmed release, and keeps a refusal or the "not confirmed in time" text.
   - The audit search is wider (§4.2.5): every `SessionActions` command call and every `.ok` read, followed through props and wrappers. It found one more consumer that the review did not name: "Resume all" (`board/FleetBoard.tsx:222-238`) counted a pending resume as resumed. **Fixed:** `resumeEach` and `resumeReport` count it apart (`Resumed 1 of 3. 1 not confirmed yet`), §4.2.3 item 10. The other `.ok` readers are not session commands (drive requests, the terminal hold) and are listed as "No change".
   - Pending-then-refused regressions: `onConfirmed runs its action only on a confirmed success` (`follow.test.ts`); `a pending release is followed until the station refuses it` and `only a confirmed release closes the dialog` (`shell.test.tsx`); `a pending prompt that the station refuses keeps its shell chip` and `a pending release that the station refuses stays in the dialog` (`e2e/shell-bang.spec.ts`); `resume all counts only a confirmed resume` (`board/resumeAll.test.ts`).

   Changed: §4.2.1 items 1 and 2, §4.2.3 items 2, 9 and 10, §4.2.4, §4.2.5, §5, §7 (cross-track note on `fleet.test.tsx`; T4 owned files, changes, tests and acceptance; T4 is now ≈ 620 lines), §8 item 19, DL-513.

Not fixed: none. The task ids do not change. T4 grows, but it stays one task: the new items use `finalOutcome` and `onConfirmed` from `commands.ts`, which T4 owns, so a split would add a dependency and no parallel work. The owned files of T1 to T4 stay disjoint; T4 does not change `web/src/board/fleet.test.tsx` (last-seen T2 owns it).
