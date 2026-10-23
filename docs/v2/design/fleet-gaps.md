# Fleet gaps G1, G2, G4 and G5

- **Status:** Draft for the architect, revision 1 (2026-10-23). Risk: **high** (§6: G5 has a deck migration). Adversary DESIGN REVIEW before the design is frozen, PRE-PR REVIEW before the merge.
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
   //! the session, because request ids are per session (DL-402). The old
   //! rows have no session and are dropped: the first scan after the deck
   //! start is a baseline that records every open request again, and a
   //! restored request older than the 60 s window never wakes.

   /// The V49 migration.
   pub(in crate::store) const SQL: &str = "
   DROP TABLE wake_seen;
   CREATE TABLE wake_seen (
       station_id     TEXT    NOT NULL,
       session_id     TEXT    NOT NULL,
       request_id     TEXT    NOT NULL,
       first_seen_ms  INTEGER NOT NULL,
       PRIMARY KEY (station_id, session_id, request_id)
   ) STRICT;
   CREATE INDEX wake_seen_first_seen ON wake_seen(first_seen_ms);
   ";
   ```
   `wake_state` (the persisted suppression) does not change. `DROP TABLE` also drops the old index. Reversal: drop the table and create the V25 shape (no rows to keep).
3. **List entry.** Append one line at the end of the `migrations()` list in `schema.rs` (after the V48 line of wall-control):
   ```rust
   M::up(super::wake_seen::schema::SQL), // V49 (G5): the wake key has the session.
   ```
   One line only: `schema.rs` is at 795 lines and wall-control also adds to it.
4. **Store.** `wake_seen.rs`:
   - `fn reserve(conn, station_id: &str, session_id: &str, request_id: &str, now: i64)`; the insert becomes `INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING`. The eviction, the suppression and the answers do not change.
   - `pub async fn wake_reserve(&self, station_id: &str, session_id: &str, request_id: &str, now_ms: i64) -> WakeReserve`. Doc: "Records that the wake rule observed `(station_id, session_id, request_id)`."
   - Module doc step 5 and the type doc keep their meaning; add after line 20: "//! The key is `(station_id, session_id, request_id)` (V49, G5): request ids are per session."
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
| The deck starts on V49 | `wake_seen` is empty. The first scan is a baseline (`scan.rs:130-140`, `self.first`): every request on a card is reserved and never wakes. |
| A request was reserved before the upgrade, the process stopped before the wake, and the station restores it later as a new event | It is reserved again. It wakes only if `requested_at` is inside the 60 s window (`limits.window_ms`). A restart for an upgrade takes longer than that in practice. Accepted (§8 item 3). |
| Two sessions ask with the same id at the same time | Two keys, two reservations, one job for the monitor (the candidates join), one wake. |
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
      * G4: set when the deck answered `pending`. It reads the command until the station decides and
      * resolves with that outcome, or with `null` when it stops (aborted, 120 s, 3 failed reads).
      */
     readonly later?: (signal: AbortSignal) => Promise<Outcome | null>;
   };
   ```
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
   ```
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

1. New types and pure helpers:
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

   export type DockRefusal = { readonly request: string; readonly text: string };

   /** G4: a refused answer whose request is still open is shown in the dock, under the request. */
   export function dockRefusal(o: Shown | null, open: readonly OpenRequest[]): DockRefusal | null {
     if (o === null || o.ok || o.action !== 'answer' || o.target == null) return null;
     const target = o.target;
     return open.some((r) => r.request_id === target) ? { request: target, text: shownText(o) } : null;
   }

   /** G4: the dock's answer handler; it names the request as the target of the action. */
   export const answerVia =
     (c: Pick<Controller, 'act'>, actions: SessionActions | null | undefined) =>
     (request: string, option: string, reply: Reply | null) => {
       if (actions) c.act('answer', () => actions.answer(request, option, reply), request);
     };
   ```
2. `Controller`: `outcome: Shown | null`; `act(name: string, call: () => Promise<Outcome>, target?: string): void`.
3. `useSessionController`:
   - Keep the shown outcome in state and in a ref (`shownRef`) with one setter `show(next: Shown | null)` that sets both.
   - Keep `follow = useRef<AbortController | null>(null)`.
   - The existing `useEffect` on `sessionId` returns a cleanup `() => follow.current?.abort()` (the follow ends on a session change and on unmount).
   - `act(name, call, target)`:
     ```ts
     const started = current.current;
     follow.current?.abort();
     const ctl = new AbortController();
     follow.current = ctl;
     setBusy(name);
     void (async () => {
       let result: Outcome;
       try {
         result = await call();
       } catch (error) {
         const message = error instanceof Error ? error.message : String(error);
         result = { ok: false, text: `Not delivered: ${message}` };
       }
       if (current.current !== started) return;
       const first = nextShown(shownRef.current, name, target ?? null, result);
       show(first);
       setBusy(null);
       if (result.later === undefined) return;
       const final = await result.later(ctl.signal).catch(() => null);
       if (final === null || ctl.signal.aborted || current.current !== started) return;
       // A success after a dismiss or a newer outcome stays quiet; a refusal always shows.
       if (final.ok && shownRef.current !== first) return;
       show(nextShown(null, name, target ?? null, final));
     })();
     ```
   - `dismiss` becomes `() => show(null)`. The session-change reset sets `show(null)`.
   - `busy` is `null` while the board follows: the owner can act again.

#### 4.2.3 The surfaces

1. `web/src/session/ApprovalDock.tsx`: new optional prop `readonly refusal?: DockRefusal | null;`. In each `<li>`, as its last child:
   ```tsx
   {props.refusal?.request === r.request_id ? (
     <p className="fd-q-error" role="alert">
       {props.refusal.text}
     </p>
   ) : null}
   ```
   `.fd-q-error` exists (`web/src/styles/home.css:461-466`, loaded globally by `web/src/main.tsx:23`); it is the rail's refusal line. No new CSS, token or class.
2. `web/src/session/SessionView.tsx` (797 lines; the change must not add lines):
   - `:40` `import { answerVia, dockRefusal, useSessionController } from './controller';`
   - `:560-563` (the four-line `onAnswer`) becomes `onAnswer={answerVia(controller, actions)}`.
   - After `busy={busy !== null}` (`:559`) add `refusal={dockRefusal(controller.outcome, s.open_requests)}`.
3. `web/src/session/SessionHeader.tsx`:
   - `:184` becomes `const outcome = dockRefusal(controller.outcome, s.open_requests) === null ? controller.outcome : null;` (a refusal that the dock shows is not shown twice).
   - `:753` becomes `<span>{shownText(outcome)}</span>`.
   - Import `dockRefusal` and `shownText` from `./controller`.
4. `web/src/board/NeedsYouRail.tsx`:
   - New exported pure function `export const railKey = (key: SessionKey, requestId: string) => `${key.station_id}/${key.session_id}/${requestId}`;`.
   - `pending` holds rail keys; `errors` becomes `Readonly<Record<string, Shown>>` keyed by rail key.
   - `answer(actions, key, request, optionId)`: `const k = railKey(key, request.request_id)`; on the outcome: `settle(k)`; when `!outcome.ok` set `errors[k] = nextShown(errors[k] ?? null, 'answer', k, outcome)`; when `outcome.later` is set, `outcome.later(life).then((final) => { if (final !== null && !final.ok) setErrors((e) => ({ ...e, [k]: nextShown(null, 'answer', k, final) })); })`.
   - `life`: `const life = useRef<AbortController | null>(null); useEffect(() => { const c = new AbortController(); life.current = c; return () => c.abort(); }, []);` and the signal is `life.current?.signal ?? AbortSignal.abort()`.
   - The keyboard path (`:149-154`) and the render (`:206-207`) use `railKey(item.key, …)` / `railKey(w.key, r.request_id)`; the error line renders `shownText(error)`.
5. `web/src/watch/WatchPage.tsx:262-275`: after `setNote(o.ok ? null : o.text)`, when `o.later` is set, follow it with a signal of the component life (the same `life` pattern as the rail) and on a final `!ok` outcome call `setNote(final.text)`.

#### 4.2.4 UI text

| Case | Text | Where |
| --- | --- | --- |
| Refused answer, request still open | `Refused: <station message>` | dock, under the request (`role="alert"`) |
| The same refusal again | `Refused: <station message> (2 times)`, `(3 times)`, … | dock or rail, the same line |
| Any other refusal | `Refused: <station message>` (unchanged) | session header |
| Pending | `Sent. The station has not confirmed it yet.` (unchanged) | session header |
| Pending, then refused | the refusal text replaces the pending text | dock (answer) or header |
| Pending, then accepted | `Done.` replaces the pending text, only while the pending text is still shown | header |

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
| G4: the owner opens another session while the board follows | the follow is aborted; nothing is shown |
| G4: `GET /api/commands/<id>` fails three times in a row | the follow stops; the pending text stays |
| G4: an e2e fake deck without the GET route (404) | three failed reads, then the follow stops |
| G5: a deck database at V48 with 10 000 rows | V49 drops them; the suppression state stays |

## 6. Risk class

**High.** G5 is a deck migration that drops and recreates the table `wake_seen` (its rows are deleted on purpose, §3.3). The other gaps are normal: G1 adds a reject code to the receipt schema that the station, the deck, the MCP tools and the board read (additive, with the `Unknown` fallback); G2 changes the answer of the coordinator tool `fleet_view`; G4 is board code only. No authentication, authorization, money or personal data change. Review: the adversary DESIGN REVIEW on this document, then PRE-PR REVIEW of the final branch.

## 7. Build plan

Workers reset their worktree to `v2` first. Every heavy command goes through `umask 022 && sh /home/luismorales/projects/obsurvive/fleet-deck-worktrees/v2/tools/heavy.sh <command>`. Do not use `--release`, `--all-features` or a plain `cargo clean`.

The owned files of T1 to T4 do not overlap; any two can run in parallel (the machine allows two workers).

Cross-track notes:
- T1 starts after wall-control B1 (deck V48) is merged into `v2`: V49 is then the 49th entry of `migrations()`. If the architect starts T1 earlier, the brief gives the position 48 for the tests, and the architect renumbers at the merge. wall-control B1 also owns `schema.rs` and `tests/merged_migrations.rs`: do not run T1 in parallel with B1.
- last-seen T1 owns `crates/fleetdeck-deck/src/store/mod.rs`; no task here changes that file.

### T1 (backend): G5, the wake key with the session, migration V49 (≈ 260 lines)

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
  - `wake_seen_schema.rs`, `#[cfg(test)] mod tests`: `v49_empties_wake_seen_keeps_wake_state_and_keys_by_session`: `migrations().to_version(&mut conn, 48)`; insert one V25 row and set `wake_state.suspended_until_ms = 7`; `to_version(49)`; `count(*) FROM wake_seen` is 0; `suspended_until_ms` is 7; insert `('s','a','r1',1)` and `('s','b','r1',1)` succeed; a second `('s','a','r1',2)` fails; `wake_seen_first_seen` is in `sqlite_master`.
  - `wake_seen_tests.rs`:
    - `one_request_id_in_two_sessions_is_two_wake_keys`: `(s, a, r1)` Reserved; `(s, b, r1)` Reserved; `(s, a, r1)` Seen; `(t, a, r1)` Reserved.
    - `the_session_key_survives_a_restart`: a file store (`tempfile`), `(s, a, r1)` Reserved; reopen with `DeckStore::open`; `(s, a, r1)` Seen; `(s, b, r1)` Reserved.
  - `tests/desktop_wake_session_key.rs` (helpers copied from `desktop_wake_cooldown.rs`: `n`, `now`, `requested`, `off`, `until`, `answer_info`; a bound display as in `desktop_wake.rs` `bound()`):
    - `one_request_id_in_two_sessions_is_two_requests_for_the_wake`: start the task; push `requested(1, "s1", t, "r1")` and `requested(2, "s2", t, "r1")` in one `apply_events`; `until reserved == 2`; serve one `desktop.info` (off) and one `desktop.act` (ok); `until sent_ok == 1`. Then advance 31 s, push `requested(3, "s3", now, "r1")`; `reserved == 3`; serve the wake; `sent_ok == 2`. (On HEAD `reserved` stays 1: the regression.)
    - `a_reserved_key_of_one_session_does_not_block_another_session_after_a_restart`: before the task starts, `wake_reserve(station, "s1", "r1", t)` is Reserved (an earlier process); start the task; push `requested(1, "s2", now, "r1")`; serve the wake; `sent_ok == 1`. Advance 31 s; push `requested(2, "s1", now, "r1")`; `until seen == 1`; `no_request` for 300 ms; `sent_ok == 1`.
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

### T4 (ux): G4, the board follows a pending command and shows a refused answer where the owner clicked (≈ 320 lines)

- **Owned files:**
  - `web/src/session/commands.ts`, `web/src/session/controller.ts`
  - `web/src/session/ApprovalDock.tsx`, `web/src/session/SessionView.tsx`, `web/src/session/SessionHeader.tsx`
  - `web/src/board/NeedsYouRail.tsx`, `web/src/watch/WatchPage.tsx`
  - `web/src/session/follow.test.ts` (new), `web/src/session/approvalDock.test.tsx`, `web/src/board/rail.test.tsx`
  - `web/e2e/session.spec.ts`
- **Changes:** §4.2. Load the `fleet-design-system` skill first (board UI rule); the design uses only the existing `.fd-q-error` line. `SessionView.tsx` must stay at or below 797 lines. In `web/e2e/*.spec.ts`, if a test fails only because a follow `GET /api/commands/<id>` is in `deck.calls`, add `&& c.body !== null` to its filter; change no other e2e line outside `session.spec.ts`.
- **Tests:**
  - `follow.test.ts` (injected `wait` that records the ms and resolves at once, and an injected `now`):
    - `a pending command is read until the station refuses it`: GET answers pending, pending, done with a rejected receipt (message `no such request`) → `{ ok: false, text: 'Refused: no such request' }`; three GETs of `/api/commands/c1`; waits `[1000, 1000, 1000]`.
    - `the reads slow down after ten`: 11 pending answers, then done → the 11th wait is 5000.
    - `an abort stops the follow`: abort after the first read → `null`, no second GET.
    - `three failed reads in a row stop the follow; a good read resets the count`: throw, throw, done → `Done.`; throw ×3 → `null`.
    - `the follow stops after 120 s`: `now` moves 121 s after the first wait → `null`.
    - `sessionActions follows only a pending answer and only with a reader`: a pending reply with `get` → `later` is a function; with `get = null` → no `later`; a done reply → no `later`.
    - `nextShown counts a repeat of the same refusal`; a different text, target or action resets to 1; `shownText` adds ` (2 times)`.
    - `dockRefusal shows a refused answer of an open request only`: accepted → `null`; action `send` → `null`; target not in `open` → `null`.
  - `approvalDock.test.tsx`: `a refusal is shown under its request only`: two requests, `refusal={{ request: 'r2', text: 'Refused: no such request (2 times)' }}` → one `role="alert"` line, inside the `data-request="r2"` item.
  - `rail.test.tsx`: `the rail keys a request by station, session and request id`: `railKey({ station_id: 'a', session_id: 's1' }, 'req-3') !== railKey({ station_id: 'a', session_id: 's2' }, 'req-3')`.
  - `web/e2e/session.spec.ts` (new tests at the end, with the file's helpers):
    - `a refused answer in the session dock shows its text under the request, and a repeat counts`: `approval.answer` answers `receipt(id, 'done', 'no such request')`; open the session; click Allow in the region "Needs you"; the dock shows the alert `Refused: no such request`; click Allow again; the alert reads `Refused: no such request (2 times)`; the session header has no alert with that text.
    - `a pending answer is followed until the station refuses it`: `approval.answer` answers `receipt(id, 'pending')`; `deck.route('GET', /^\/api\/commands\/([^/]+)$/, (m) => ({ status: 200, body: receipt(m[1], 'done', 'no such request') }))`; click Allow; the dock alert `Refused: no such request` appears within 5 s.
- **depends_on:** none (no generated type changes; it does not read the new reject code).
- **Acceptance:**
  - `cd web && bun run typecheck && bun run lint`
  - `cd web && bun run test src/session src/board src/watch` (through heavy.sh)
  - `cd web && bun run e2e session.spec.ts revert.spec.ts` (through heavy.sh)
  - `sh tools/design-check.sh`
  - The guards (as T1).
  - Gallery shot of the session view with an open approval, both themes, desktop and phone (the fleet-design-system skill §3 step 3): the refusal line under the request.

## 8. Decisions made in this draft

1. **G1: a new reject code `outside_roots`**, not `forbidden` with a better text. A caller can act on a code (pick another folder) and tell it from a grant refusal. The word matches the existing link error code `outside_roots`. Rejected: `cwd_outside_roots` (a second word for one meaning); a link feature flag (the receipt schema has an `Unknown` fallback).
2. **G1: the refusal text names the folder that the caller sent and the station name**, not the canonical path and not the allowed roots. Rejected: listing the roots (it shows the owner's folder layout to every caller; the owner sets the roots on the board).
3. **G1: the outbox reason carries the station text** (`rejected: <code>: <text>`, at most 300 characters, control characters as spaces). One change fixes `command_status`, the task rows and the mail rows. Rejected: a new `reject` object in `command_status` (a second place for the same fact; the task and mail rows would stay without the text).
4. **G1: no check on the deck.** The deck does not know the roots and cannot resolve symlinks on the station. Rejected: send the roots in `StationInfo` (a new field in every link handshake for one error text).
5. **G2: the filter applies to `stations`, `projects` and `sessions`**; `me` ignores it. Rejected: sessions only (the schema already says "Station id or name." for every view).
6. **G2: a filter that names no station, or an ambiguous name, is `invalid_params`.** Rejected: an empty page (a typo looks like an empty fleet).
7. **G5: the old `wake_seen` rows are dropped** in V49. They have no session, and the first scan after the deck start is a baseline. Rejected: keep them with an empty session and treat a match as `Seen` for 24 hours (provider ids `req-1`, `req-2` repeat in every session, so this would keep the bug for a day after the upgrade); derive the session from the cards (the cards of the moment do not hold every old request).
8. **G5: the migration file is declared from `wake_seen.rs`, and the list entry is one line.** `store/mod.rs` (796) and `schema.rs` (795) are near the 800-line limit.
9. **G4: the report's cause is corrected** (§4.1): the board gets the receipt in the HTTP answer. The fix places the refusal where the owner clicked, counts a repeat, and follows a command that is still pending after 5 s.
10. **G4: the board reads `GET /api/commands/<id>`** (1 s ×10, then 5 s, at most 120 s, 3 failed reads stop it). Rejected: a new board WebSocket event for commands (a deck and protocol change for a rare case); a longer `RECEIPT_WAIT` (it holds an HTTP request and the button for longer).
11. **G4: the dock shows a refused answer under its request**, and the header does not show it twice. Rejected: a toast (no toast primitive in the design system); the header only (the case that the owner missed).
12. **G4: a repeat is counted in the text** (`(2 times)`). Rejected: an animation (motion rules of the design system; a screen reader does not see it).
13. **G4: the rail keys its state by station, session and request id**, the DL-402 rule on the board.
14. **G4: a success after a dismiss stays quiet; a refusal after a dismiss shows.**

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
- Decision: `sessionActions.run` gives a `pending` outcome a `later` follow-up that reads `GET /api/commands/<id>` (1 s ×10, then 5 s, at most 120 s; three failed reads stop it). The session controller, the rail and the watch tiles show the final refusal.
- Rejected: a WebSocket event for commands; a longer `RECEIPT_WAIT`.

## DL-514: G4 a refused answer is shown under its request in the dock, and a repeat is counted (design agent, 2026-10-23)

- Decision: the approval dock shows a refused answer under the request with `.fd-q-error`; the session header does not repeat it. The same refusal again reads "… (2 times)". The rail keys its pending and error state by station, session and request id.
- Rejected: a toast; the header only; an animation.

## DL-515: G5 the wall-wake key has the session; deck migration V49 rebuilds `wake_seen` empty (design agent, 2026-10-23)

- Context: request ids are per session (DL-402, station V43). The deck wake scan and `wake_seen` keyed requests by `(station, request_id)` (`wake/scan.rs:149`, `display_v25.rs:43-49`), so the second session with the same id never woke a wall.
- Decision: the key is `(station_id, session_id, request_id)` in `Scanner.known`, `present`, `wake_reserve` and the table. V49 drops and recreates `wake_seen` with the new primary key; `wake_state` stays. The first scan after the start is a baseline, so no open request wakes after the upgrade.
- Rejected: keep the old rows with an empty session (the bug stays for 24 hours); derive the session of old rows from the cards.
