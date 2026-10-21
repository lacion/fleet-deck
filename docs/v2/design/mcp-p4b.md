# MCP P4b — live grant changes and live tool lists

- **Status:** Accepted revision 5, 2026-10-20. DESIGN RE-REVIEW round 3 (the last) FAIL with 1 BLOCKER on revision 4 (cca63bfc; `~/fleetdeck-briefs/mcp-p4b-design-r3-adversary.md`): the 5 s bound of §13 did not hold. Owner decision at the review limit (2026-10-20): **the late check** (§7.1), map §14.4. Earlier: revision 4 (cca63bfc) for round 3. Revision 3 (2376c5ae): RE-REVIEW round 2 FAIL, 1 BLOCKER and 3 MAJOR (`~/fleetdeck-briefs/mcp-p4b-design-r2-adversary.md`, map §14.3). Revision 1 (997b107d): advisor review (`~/fleetdeck-briefs/mcp-p4b-advisor-r1.md`, map §14.1). Revision 2 (6a69bdbe): DESIGN REVIEW round 1 FAIL, 2 BLOCKER and 10 MAJOR (`~/fleetdeck-briefs/mcp-p4b-design-r1-adversary.md`, map §14.2).
- **Program:** DL-251 (D1), DL-255 (P4a), owner decisions L1–L3 of 2026-10-20 (`~/fleetdeck-briefs/mcp-p3-research-notes.md` §"P4b owner decisions").
- **Evidence:** spike `spikes/s-events/` (HTTP C1–C5, X1–X3; stdio C6–C8). Claude 2.1.302 re-lists its tools 2–7 ms after `notifications/tools/list_changed`: over HTTP only on a `subscriptions/listen` stream (C1; no TTL refetch, C2), over stdio on a listen stream **or** as an unsolicited line (C6, C7). Codex 0.163.0 never re-lists inside a running process (X1A–X1C).
- **Out of scope:** live changes of the `desktop`, `capture` and `input` classes, of their caps and allowances, of a grant's scope or stations; `resources/updated`, prompt list changes, progress and cancellation; Codex live tool lists (impossible in 0.163.0); a binary downgrade of a station past its migration V42 or of a deck past its P4b migration (§9).

## 1. Goals

1. **Give (L1).** The owner gives a grant to a session that has no owner grant in effect (running or not: a session started without one, a resumed session, a session started before P4a, a session with a worker grant).
2. **Change (L1).** The owner adds or removes the classes `read`, `mail`, `drive`, `spawn`, `inspect`, `manage`, `answer` of a session's owner grant. Every widening needs the owner's elevation, as at creation.
3. **Live tool list.** A running Claude session sees a change within seconds on both hosts (direct HTTP and shim stdio, L3).
4. **Grants outlive processes (L2).** An owner grant that has a record (§2) belongs to the session, not to one provider process: when the process ends and the session starts again, the new process gets the grant at its start. So Codex sees a change at its next start.
5. **The deck stays the authority.** Every class-gated call is decided by the deck from the stored grant. A narrowing or a revoke stops every **fleet-control command** at the station's pre-execution authority check (§7). Ordinary work (mail, tasks, drive, spawn) that the deck admitted before a narrowing or a revoke completes, exactly as after a revoke today (§7.3).

## 2. The model: one desired state per session, ordered by one sequence

- **Deck.** A new table `session_owner_grant(station_id, session_id, grant_id, seq INTEGER NOT NULL, station_seq INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (station_id, session_id))`. `seq` is a **per-session** counter: every give, change and end of an owner grant of that session takes the next value; it never goes back, also across different grants. `station_seq` is the highest `seq` that the station acknowledged.
- **Station.** A new durable table `session_grants` (station migration V42): `session_id PRIMARY KEY`, `grant_id`, `seq`, `spec` (JSON: the station form of the `CoordinatorSpec`: classes, scope, control skill, `expires_at`, kind, the allowances that `grant_classes` reads), `generation` (the binding generation of the latest process of this record), `state` (`attached` | `ended`), `ended_reason`, `updated_at`. It is independent of `shim_bindings` and of shim cleanup.
- **The binding of a running process is derived from the record**, at every process start and on every accepted change. Credentials keep their own lifecycle (§4).
- Records exist only for grants that a `coordinator_live.v1` deck gave or started (§9); a grant without a record follows every rule of today.

## 3. One command: `session.coordinator_set`

- Proto: `SessionCommand::CoordinatorSet { session_id, seq: u64, grant_id, spec: Option<Box<CoordinatorSpec>> }` (`session.coordinator_set`). `Some(spec)` is the full desired state of that grant (give or change); `None` ends it (revoke). The existing `session.coordinator_revoke` stays for grants without a record.
- **Station rules,** in the decision transaction:
  1. `guard` (paired deck) and `owner` (origin `Board`); `spec.kind` must be `coordinator`; a give may not carry `desktop`/`capture`/`input` classes, and a change may not change their membership against the record (`invalid_grant`).
  2. `seq <= record.seq` → accepted as stale, no change (the receipt names the record's `seq`). A new session has `record.seq = 0`.
  3. `seq > record.seq`: write the record (`attached` with this grant and spec, or `ended` with `ended_reason = revoked` when `spec` is `None` and the grant is the record's). When the record held **another** grant that is still `attached`, that grant ends in the same transaction (`coordinator.ended { old binding, generation, "replaced" }`); the deck already ended it (§6.1).
  4. Apply the record to the running binding, if any (§4), in the same transaction (shim: the row rewrite; direct: memory after the commit).
  5. The receipt's result carries the record's `seq`; the deck sets `station_seq = max(station_seq, seq)`.
- Ordering and reconcile: the deck keeps at most one pending `coordinator_set` per session (a newer one supersedes and cancels the older pending command in the same transaction). When a link that negotiated `coordinator_live.v1` comes up, the deck queues the latest desired state of every session of that station whose `station_seq < seq` and that has no pending set. Because one sequence orders every grant of the session, a delayed message of a replaced or ended grant can never restore it.

## 4. The running binding

### 4.1 In-place changes keep the credential
The provider process holds its token in its environment, so a change of a running session never mints a credential. The binding entry under the same digest is rewritten:
- **attached record:** `binding_id = "{s}:{grant}"`, `grant_id`, `classes = grant_classes(record.spec, features)`, `scope`, `control_skill`, `kind`, `expires_at` (a new field of `Binding`). The native identity and the state stay: `Pending` stays `Pending`, `Active` stays `Active`.
- **ended record:** the entry becomes the session's fleet-only binding in place (`"{s}:fleet"`, empty `grant_id`, classes `{Own}`, scope `Lineage`, the fleet skill, no expiry, every internal allowance cleared), state unchanged: exactly the grant-less tool set (D2).
- **A worker binding** that a give replaces is rewritten the same way in the same transaction, with `coordinator.ended { worker binding, generation, "replaced" }`; the deck sends no separate worker revoke for a replacement (§6.1).
- **Shim host, durably:** a new store function rewrites the `shim_bindings` row of the digest (`binding_id`, `grant_id`, `classes`, `scope`, `control_skill`, `kind`, `expires_at`) in the decision transaction; memory is published only after the commit; a failed save leaves the old entry gated (`starting`) until the next reload. Recovery loads the rewritten row, never the old authority.
- **Events:** the first attach of a grant to an `Active` binding of this process appends `coordinator.bound { binding_id, grant_id, generation, … }` (the generation of §5); a change appends nothing; an end appends `coordinator.ended { binding, generation, "revoked" }`.
- **Never** rewritten in place: a `Revoked` or `Expired` entry, an entry whose shim row is `ended` or `suspended`, a fenced digest. Those stay dead; the session gets the record at its next process start.

### 4.2 Hard invalidation stays
Rotation (resume), the process end, a fence, a changed native identity, a station-local refusal and the station's own expiry check keep killing the credential exactly as today. Only the end of a grant **with a record** becomes the in-place downgrade. A leaked token therefore keeps fleet-only access until its process ends (as a grant-less session) and would see a later give to that session; accepted (the token never leaves the provider's environment).

## 5. Process starts and generations (L2)

- **One generation counter per grant**, owned by the record (`record.generation`) and acknowledged by the deck (`tokens.bound_generation`). It moves forward on two paths only: a process start and a renewal.
- **Process start.** At every process start of a session whose record is `attached` and not past `spec.expires_at`, `tools_generic` prepares the grant binding (not the fleet-only one) with a new credential, `generation = record.generation + 1`, state `Pending`, and stores the new generation in the record. Shim host: `store_tools` with the grant row; `rotate_out` ends only rows of earlier processes of the session (reason `rotated`, no `coordinator.ended` for a grant with a record). A record past its expiry ends (`ended_reason = expired`, `coordinator.ended "expired"`) and the process gets the fleet-only binding.
- **Bind.** When the native identity is known the binding becomes `Active` and the station appends `coordinator.bound { generation }`. A resume expects the session's stored native id; a mismatch revokes the binding, ends the record (`ended_reason = identity_changed`) and appends `coordinator.ended { binding_id, generation, "identity_changed" }` (§6.4 handles it on the deck). Until the identity is verified the binding is `Pending` and every gated call is refused (as today).
- **Process end.** The binding ends as today (credential dead). For a grant with an `attached` record the station appends **no** `coordinator.ended`; the record stays attached.
- **Deck rebind rule** (`apply_bound`): a `coordinator.bound` for a grant with a `session_owner_grant` entry, from the grant's station and session, with `generation > bound_generation`, rebinds the grant **when the grant is in effect** (active, inside its lifetime, not idle-expired): `bound_generation = generation`, `invalidate_stale_generation`, the usage counters and the idle clock kept. A grant not in effect ignores it (the owner renews, below). Every other higher generation needs the owner's pending renewal, as today. No detach event and no snapshot change are needed: a missed process end changes nothing, because the next process's higher generation rebinds.
- **Renewal.** The owner renews an idle-expired grant as today; the deck's pending renewal is `p = bound_generation + 1`. The station's renew of a grant with a record sets `record.generation = g = max(p, record.generation + 1)`; when a process of the session holds the binding, its generation becomes `g` and it appends `coordinator.bound { g }`; otherwise the record keeps `g − 1` as its base so that the next process start binds at `g`. The deck accepts a renewal bound with `generation >= p` (instead of `== p`) for grants with a record.
- **Snapshot projection (round 2 MAJOR).** For a grant with a record, the station's coordinator binding projection (the snapshot of a coordinator-capable link) follows the **record**, not the credential: an `attached` record with a live credential (`Pending` or `Active`) reports that credential; an `attached` record whose credential is dead (process ended, rotated, not started yet) reports **nothing** for that grant (the deck's `apply_projection` leaves a grant that is absent unchanged); an `ended` record reports the grant as `Revoked` (`Expired` for `ended_reason = expired`), so a missed terminal event reaches the deck; such a synthetic terminal entry goes through the record-aware terminal rule of §6.4 (`generation >= bound_generation`, by binding id), not the exact-generation rule. Grants without a record keep today's projection. Covered: recovery while hibernated, after a crash, and after an identity failure.
- **Stale work:** every deck coordinator operation is keyed by `(grant_id, generation)`; a rebind runs `invalidate_stale_generation`, and the station's authority check (§7.1) compares the generation.

## 6. Deck routes and lifecycle

### 6.1 Give: `POST /api/coordinator/sessions/{station_id}/{session_id}/grant`
- Owner login only (`403 owner_only` for a member or a token). Body: the board's coordinator request (the JSON of `spec.coordinator` of a board start) without desktop, capture, input, recording or click fields (`400 invalid_grant` "desktop, screenshot and input access is given only at start").
- Checks: the station link negotiated `coordinator.v1` and `coordinator_live.v1` (`409 not_supported`); the session is known on the deck; then `check_owner_grant`, the creation checks moved out of `owner_start` (classes, `take_spawn_fields`, target stations, the elevation rule, `coordinator.v2` for fleet control).
- **One owner grant per session:** in the transaction, a `session_owner_grant` row whose grant is in effect **or idle-expired** → `409 has_grant` (renew or revoke it first). A row whose grant is terminal (revoked, lifetime over) is overwritten.
- In one transaction: create the grant (`session_id` set, `start_command_id` NULL), write `session_owner_grant` with the next `seq`, mark a worker grant in effect for the session revoked (`replaced`) **without** queueing a worker revoke (the station ends it inside the set, §4.1), queue `coordinator_set { seq, grant, spec }` (commands table; the router holds it until the link negotiated `coordinator_live.v1`), audit `coordinator.grant_created` with `live=true`.
- A failed or rejected give (`commands::finish`) revokes the grant (`attach_failed`); the next give overwrites the row.

### 6.2 Change: `POST /api/coordinator/grants/{grant_id}/classes`
- Owner login only. Body `{ classes, spawn_cap?, max_modes?, answer_cap? }`. The grant: kind `coordinator` (`409 worker_grant`), not terminal, has a `session_owner_grant` row (a grant without a record: `409 not_supported`).
- Rules: the `desktop`/`capture`/`input` membership unchanged; at least one class; a newly added `spawn` requires `spawn_cap` and `max_modes`; a newly added `answer` requires `answer_cap`; a new fleet-control class needs `coordinator.v2` on every station of the grant and the fleet-control switch; caps fields only with their newly added class (other cap changes use the caps route).
- **Widening, compared with the stored row in the transaction:** any added class; a cap higher than the stored one; a new provider family in `max_modes`; a ceiling that is not `mode_within` the stored one (the rules of `grant_spawn_caps.rs`). Widening needs elevation.
- In one transaction: write the classes and caps (usage counters never reset; a removed and re-added class keeps its counters), the next `seq`, audit `coordinator.grant_classes` (old and new), `sync_coordinate_scopes`, apply §7.2, and queue `coordinator_set { seq, grant, spec }` superseding any pending set.

### 6.3 Revoke
Unchanged route. For a grant with a `session_owner_grant` row: the next `seq`, the grant revoked, §7.2, and `coordinator_set { seq, grant, None }` instead of `session.coordinator_revoke`.

### 6.4 Terminal events from the station
- For a grant with a `session_owner_grant` row, a `coordinator.ended` from the grant's station with `binding_id = "{s}:{grant}"`, a terminal reason (`identity_changed`, `expired`, `revoked`, `replaced`) and `generation >= bound_generation` makes the grant terminal (revoked with that reason, `expired` for `expired`), applies §7.2, and leaves the row for the next give to overwrite. (Today's exact-generation rule stays for grants without a row.)
- The late-bound rule of P4a (a `coordinator.bound` for a terminal grant queues an end) extends to owner grants with a row: it queues `coordinator_set { next seq, grant, None }`.

## 7. Narrowing and execution

### 7.1 Fleet-control commands: the station's pre-execution check
- Every outbox item and every `land_queue`/move item that carries a `CommandAuthority` also stores, deck-side only, the **full class set** that admitted it (`outbox.required_classes`, the same in the land and move tables; JSON array; the admitting decision's class requirements, for example `{spawn, manage}` for `project_start`).
- `authority_local` (the deck's answer to the station's `coordinator.authority_check` before a fleet command runs, ADR-008 §6.2) and its land and move fallbacks also require `required_classes ⊆ grant.classes` (besides today's state, generation, coverage and switch). A narrowing or a revoke therefore stops any fleet command whose authority check has not been answered yet, also one already sent: the station executes only after the deck says yes. A check that the deck answered `valid` **before** the narrowing or revoke committed is not recalled.
- **The late check (owner decision at the review limit).** On every station path that executes a fleet command (a launch: `manager/fleet_control.rs`, the shim start in `manager/shims.rs` with `store_tools` and the provider's executable hashing in `fleetdeck-providers/src/host/shim/start.rs`, the direct start; a write: `manager/fleet_git_writer.rs` and the land and move steps), the station's **final** `coordinator.authority_check` runs after every awaited preparation step (credential save, executable hashing, workspace preparation, queue waits) and immediately before the launch or the write; an earlier check may stay as a fast refusal, but it never authorizes the effect. Between the final `valid` and the spawn or the write there is no further await except the effect itself. The remaining window is that gap (§13). This also hardens revoke.
- Items admitted before this change (no `required_classes`) are checked as today.

### 7.2 Narrowing fails pending items at once
A narrowing (§6.2), a revoke or a terminal event (§6.4), in its own transaction, fails every pending outbox, land and move item of the grant whose `required_classes` is no longer a subset (`authority_removed`), so it is never sent.

### 7.3 Ordinary work
Ordinary intents (mail, tasks, drive, spawn without fleet control) keep today's semantics: they carry no `CommandAuthority`, the deck admits them against the grant at call time, and work that was admitted before a narrowing or a revoke completes (as after a revoke today). The fleet-control rules (`fleet_control_off`, `coordinator.v2`, strict targets) still apply only to fleet commands.

## 8. Live tool list transport

### 8.1 A change signal bound to the credential
- `authenticate` returns, under the bindings lock, a stable handle: the digest, an entry incarnation (bumped by `insert`), and a `watch::Receiver<u64>` registered at the same moment. `BindingExt` carries the handle as well as the snapshot.
- In-place changes (§4.1) bump the version and keep the watch open. Hard invalidation (§4.2) closes the watch; every listen or watch stream on it ends. Receivers coalesce.

### 8.2 HTTP: rmcp's `subscriptions/listen`
- `get_info` adds `.enable_tool_list_changed()`. `StationMcp` implements `accepted_subscription_filter` (only `toolsListChanged`) and `listen(ctx)`: it takes the handle from the request extensions, sends **one `notify_tool_list_changed()` right after every acknowledgment** (the client re-lists once; this covers every change before the listen, also a change before the reconnect authenticated), then loops: version change → notify; watch closed → return `Ok(())` (rmcp's final result); `ctx.cancelled()` → return.
- `McpService::serve` returns, for a modern `subscriptions/listen` only, rmcp's response with its headers (`Content-Type`, `Cache-Control`, `X-Accel-Buffering`) and its body unbuffered; every other request keeps today's collected reply and `PermitSlot` flush rule. The ingress response body becomes `Either<Full<Bytes>, BoxBody>`.
- **Admission:** authentication and the rate check are separate from the 8 ordinary request permits. A listen takes a listen permit (2 per binding, 256 per station, atomic) that the body owns for its whole life; the unauthenticated intake permit is released after the handshake. The connection limit becomes 512, of which listens hold at most 256.
- **Stalls:** a frame that cannot be written within 30 s ends the stream; rmcp's 15 s keep-alive stays; no lifetime limit for a healthy listener.
- A legacy-era `subscriptions/listen` keeps today's method-not-found answer.

### 8.3 Stdio: a push line through the bridge
- **Socket.** First line `WATCH <token>`. Answer: `OK watch.v1` (registered with the handle of §8.1), `ERR unauthorized`, `ERR starting`, `ERR limited`; an older station answers `ERR bad_request`. Limits: 2 watches per binding, 128 per station; `MAX_CONNECTIONS` 64 → 256 with at most 128 watches. After `OK`, the station writes the line `{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}` **once at once** (a refresh for every change before the registration) and then once per coalesced change; a write stalled 30 s closes; it reads with a bounded buffer and closes on EOF or any client byte; a closed watch closes the connection.
- **Bridge.** It starts the watch task only after it relayed the first successful answer to `server/discover` or `initialize`. Connect, `WATCH`, expect `OK watch.v1`; write each received line that is exactly the notification (at most 256 bytes; anything else closes) to stdout through `emit`, coalescing (at most one pending). `ERR unauthorized` or `ERR bad_request` stop the task for good; `ERR starting`, `ERR limited` or a connection error retry with backoff 1 s → 30 s plus jitter. The station's first line after every `OK` is the refresh, so the bridge never synthesizes one. The task stops when stdin closes.
- A `subscriptions/listen` through the socket is answered at once with `-32601` (before the collecting path). A shim session started before the upgrade runs the old bridge and gets no push; it gets the record's tools at its next process start.

### 8.4 Codex
Unchanged: legacy era, no listen, no re-list; the record reaches it at its next process start (§5).

## 9. Versions and feature gating

- Feature `coordinator_live.v1` (`version.rs`, after `WORKER_GRANT`). The deck sends `coordinator_set` only on a link that negotiated it; the router holds it otherwise; the give and change routes refuse with `409 not_supported`; the board hides the actions.
- A start-time owner grant of a deck link that negotiated the feature also gets a record (the deck writes a `session_owner_grant` row with `seq = 1` when the start's receipt names the session; the station writes the record when it prepares the start's grant binding, with `seq = 1`). Every new station lifecycle rule (no `coordinator.ended` at the process end, the downgrade, the rebind at a start) is tied to a record, which only such a deck creates.
- The deck's new rules (rebind, `>=` renewal, terminal events by binding id) apply only to grants with a `session_owner_grant` row.
- WATCH is negotiated by its own `OK watch.v1`; an old station never causes a synthetic refresh.
- **Downgrade:** station and deck migrations are forward-only (an older binary refuses a newer database, `DbError::TooNew`), as for every earlier migration. A binary downgrade of a station past V42 or of a deck past its P4b migration is not supported; the release notes say so (round 2 MAJOR). A link between a new station and an older deck **binary that never had P4b** is the only mixed case: that deck never negotiates `coordinator_live.v1`, so the station writes no records for it and every rule of today applies.

## 10. Authorization invariants

1. Only the owner login gives, changes or ends a grant; no coordinator tool can.
2. Every widening (class, cap, family, ceiling) needs the owner's elevation, compared with the stored row in the transaction; a removed and re-added class keeps its usage counters. Worker grants: their classes never change; their spawn cap and mode ceilings can be raised by the owner with elevation within the worker bounds (P4a §8, accepted and unchanged by P4b).
3. The deck decides every class-gated call from the stored grant. A fleet command runs only after the deck's final pre-execution check, placed after every awaited preparation step, confirms its full class set, generation and state (§7.1); a `valid` answer given before a narrowing is not recalled (§13). Ordinary admitted work completes (§7.3).
4. One owner grant per session; one per-session sequence orders every give, change and end, so a delayed message never restores a replaced or ended grant.
5. In-place changes keep the credential; hard invalidation keeps killing it; a dead credential is never revived; a new process gets a new credential and a higher generation that the deck accepts only for a grant in effect.
6. Listen and watch streams are authenticated, bounded per binding and per station, own their permits, refresh once at registration, detect stalls and EOF, and end with their credential.

## 11. Tests (names are binding)

Station:
1. `a_change_bumps_the_watch_and_hard_invalidation_closes_it` (in-place keeps it open; revoke of a credential, rotation, expiry, process end close; race-free registration with the handle).
2. `a_listen_stream_refreshes_notifies_and_ends_with_the_credential` (HTTP modern era: the ack and the refresh, a change, a downgrade keeps it, a rotation ends it with the final result; 8 busy request permits do not block a listen; the 3rd listen of a binding and the 257th of the station are refused; a stalled reader ends after 30 s; a legacy listen gets method-not-found; the socket listen gets -32601).
3. `a_watch_connection_refreshes_pushes_and_closes_with_the_credential` (OK + the first line, unauthorized, starting, limited, bad_request from an old station; EOF and client bytes close; limits).
4. `the_bridge_starts_its_watch_after_discovery_and_reconnects_with_backoff` (no line before the first answer; stops on unauthorized, bad_request and stdin close; coalescing; an oversize line closes).
5. `coordinator_set_orders_by_the_session_sequence` (stale seq ignored, a replacement ends the old grant with `replaced`, a delayed set of a replaced or ended grant is ignored, desktop classes refused, `kind worker` refused).
6. `a_set_rewrites_the_running_binding_in_place_on_both_hosts` (the same token sees the new tools; a pending binding stays pending; a worker binding replaced in the same transaction; an end downgrades to fleet-only and keeps the token; a dead credential is never rewritten; a station restart reloads the rewritten row; a failed row save stays gated).
7. `a_grant_with_a_record_returns_at_every_process_start_with_a_higher_generation` (hibernate + resume, crash + auto-continue resume, both hosts; no `coordinator.ended` at the process end; an identity mismatch ends the record with `identity_changed`; an expired record ends with `expired` and gives the fleet binding; a renewal while no process runs; a grant without a record behaves as today).
Deck:
8. `the_owner_gives_a_grant_to_a_running_or_hibernated_session` (`check_owner_grant` elevation; `has_grant` also for an idle-expired grant; a terminal grant's row overwritten; concurrent gives; a worker grant replaced without a worker revoke command; desktop fields refused; held until the feature; a failed give revokes).
9. `class_and_cap_changes_need_elevation_for_every_widening` (an added class, a higher cap with the same classes, a new family, an incomparable ceiling; counters kept across remove and re-add; worker grants refused).
10. `a_narrowing_or_a_revoke_stops_fleet_commands_before_execution` (the late check: the launch preparation is paused for more than 5 s after an early `valid`, a narrowing commits, and the launch is refused by the final check — on the shim start, the direct start and a Fleet Git write; a sent `manage` command fails the station's authority check after `manage` is removed while `inspect` stays; `project_start` under `{spawn, manage}` fails when either is removed; land and move fallbacks; pending items fail with `authority_removed`; ordinary admitted mail completes).
11. `the_rebind_rule_accepts_a_higher_generation_only_for_a_grant_in_effect` (a missed process end, a snapshot recovery while the record is attached and the credential is dead leaves the grant unchanged, a snapshot with an ended record ends the grant, an idle-expired grant ignored then renewed with `>=`; a grant without a row keeps today's rules; `identity_changed` with a higher generation revokes and fails pending items).
12. `the_session_sequence_reconciles_after_a_reconnect_and_supersedes_pending_sets` (revoke A then give B in either delivery order; a delayed A set after B; the reconnect queues the latest state).
13. `a_member_or_a_token_cannot_give_change_or_end_a_grant`.
14. Migrations: deck `session_owner_grant`, `outbox.required_classes` (and the land/move tables); station V42 `session_grants`.
Board: 15. the Fleet access form (give, change, elevation for widening, the Codex note, hidden without the feature).
Live (architect): a running Claude session on a shim station gets `spawn` from the board and lists the spawn tools without a restart; a Codex session gets them after a hibernation and resume; a revoke leaves the fleet tools.

## 12. Phases and module splits

- **P4b-0 (done, merge 5c674eeb):** the module splits.
- **P4b-1 (station transport):** §8; tests 1–4.
- **P4b-2 (station desired state):** proto §3 and the feature; station §2–§5; tests 5–7.
- **P4b-3 (deck):** §6, §7, the deck side of §5 and §9; tests 8–14.
- **P4b-4 (board):** the Fleet access form; test 15.

## 13. Accepted limits

- Ordinary work admitted before a narrowing or a revoke completes (§7.3), as for a revoke today.
- A leaked token keeps fleet-only access until its process ends (§4.2).
- No binary downgrade of a station past V42 or of a deck past its P4b migration (§9).
- A fleet command whose **final** authority check (§7.1, the late check) the deck answered `valid` before a narrowing or a revoke committed may still run; the window is the gap between that answer and the spawn or the write, with no awaited preparation in between (owner decision at the review limit, 2026-10-20; a two-phase fence was not chosen).
- A shim session started before the upgrade gets no push until its next process start (§8.3).

## 14. Review maps

### 14.1 Advisor (revision 1 → 2)
| Advisor correction | Where (revision 3) |
| --- | --- |
| 1. Elevation bypass through caps and ceilings | §6.2; test 9 |
| 2. Narrowing must stop queued work | §7; test 10 |
| 3. Do not reuse `suspended` for parking | §2 the record; §5 (no parking state) |
| 4. Explicit execution transition at a resume | §5 generations and the rebind rule; tests 7, 11 |
| 5. Durable downgrade, keep hard invalidation | §4.1, §4.2; test 6 |
| 6. Ordered desired state, one attachment per session | §2, §3 the session sequence; §6.1; tests 5, 8, 12 |
| 7. Race-free registration; downgrade keeps the stream | §8.1; tests 1–3 |
| 8. Admission, owned permits, stalls, EOF | §8.2, §8.3; tests 2–4 |
| 9. Gate lifecycle semantics; negotiate WATCH | §9, §8.3 |
| 10. Adversarial tests; module splits | §11; §12 (P4b-0 done) |

### 14.2 DESIGN REVIEW round 1 (revision 2 → 3)
| Finding | Where |
| --- | --- |
| BLOCKER — narrowing not serialized with dispatch | §7.1 the station's pre-execution check (`authority_local` and its land and move fallbacks) checks the stored class set; test 10 |
| BLOCKER — one class cannot express compound authority | §7.1 the full `required_classes` set; test 10 |
| MAJOR — caps route widens worker grants | §10.2 restated: P4a §8 lets the owner raise worker caps within the worker bounds with elevation; classes stay fixed |
| MAJOR — authority on ordinary intents breaks fleet-control rules | §7.3 ordinary intents get no `CommandAuthority`; their limit is stated (§13) |
| MAJOR — worker revoke kills the credential before the owner set | §4.1 and §6.1: the worker is replaced inside the set; no separate worker revoke |
| MAJOR — per-grant revisions do not order replacement | §2, §3 one per-session sequence; test 12 |
| MAJOR — lifetime expiry frees the deck but not the station | §5 a record past expiry ends at the next start; §6.1 a terminal grant's row is overwritten by the next give (no early release); §6.4 terminal events |
| MAJOR — snapshots cannot recover a missed detach | §5 no detach event: a higher generation rebinds a grant in effect; test 11 |
| MAJOR — identity failure in an unbound epoch | §6.4 terminal events with `generation >= bound_generation` by binding id; test 11 |
| MAJOR — renewal and process generations share a counter | §5 one counter in the record, renewal `max(p, record + 1)`, deck accepts `>= p`; tests 7, 11 |
| MAJOR — registration misses changes before authentication | §8.2, §8.3 a refresh after every acknowledgment and every `OK watch.v1`; tests 2, 3 |
| MAJOR — no downgrade path | §9 forward-only migrations, no binary downgrade past V42; deck downgrade harmless; §13 |

### 14.3 DESIGN RE-REVIEW round 2 (revision 3 → 4)
| Finding | Where |
| --- | --- |
| BLOCKER — a `valid` authority answer does not fence later execution | Accepted limit (§7.1, §10.3, §13): the window is the check round trip (≤ 5 s), as for a revoke today; the architect did not adopt a two-phase execution fence (it would delay the confirmation, not the command) |
| MAJOR — worker widening through `/caps` (round 1 M1) | §10.2: the P4a policy (the owner raises worker caps and ceilings with elevation within the worker bounds) is the accepted rule; the review criterion is aligned |
| MAJOR — a snapshot reports a dead credential of an attached record as revoked | §5 snapshot projection follows the record; test 11 |
| MAJOR — the deck downgrade is not harmless | §9: deck binary downgrade past its P4b migration excluded, as for the station; §13 |

### 14.4 DESIGN RE-REVIEW round 3 (revision 4 → 5, at the review limit)
| Finding | Where |
| --- | --- |
| BLOCKER — the 5 s bound does not hold: execution can follow `valid` after awaited preparation | Owner decision: the late check (§7.1): the final authority check runs after every awaited preparation step, right before the launch or write; §13 restated; test 10 |
| Note — synthetic terminal snapshot entries | §5: they use the record-aware terminal rule of §6.4 |

## 15. Implementation notes (architect, 2026-10-21)

Decisions made while building, at the level of the design (the briefs in `~/fleetdeck-briefs/mcp-p4b*-brief.md` hold every detail):

1. **Transport (P4b-1).** The listen permits are owned by the response body; the 30 s stall rule is in the connection IO (`poll_write` and `poll_write_vectored`, one deadline); `end_session` and `revoke_session` close the change signal too; `WATCH` takes the unauthenticated intake permit like `AUTH`. The bridge tests with a real socket run in real time (paused tokio time fires timeouts during socket I/O); the backoff has a pure unit test.
2. **The late check (P4b-2d).** Only the shim launch needed it (the direct start and the Fleet Git writes already checked last): the provider's `start_with` is split into `exe_digest` and `start_with_digest`, and the station's final check runs between them, after the credential save.
3. **The station record (P4b-2b).** `spec` is nullable: an end for a session without a record writes an `Ended` record, so a delayed give can never apply. A set whose spec names another grant is refused. The running binding is rewritten only when its target form differs from it (decision and publish use the same rule); the shim row is rewritten in the decision transaction and memory after the commit, and memory never gets ahead of the row. An unreadable fence blocks the rewrite (fail closed).
4. **Process starts (P4b-2c).** A start that writes a record answers `result.seq = Some(1)`; the deck creates its seq-1 row from that receipt. A changed native identity of any binding of an attached grant ends the record (`identity_changed`). A renewal of a `Pending` binding is refused. The pre-existing station renew could never succeed (the station never set `Expired`); it now works for grants with a record and is unchanged for grants without one.
5. **Deck authority (P4b-3a).** `revoke_tx` (every revoke) now fails the pending fleet items of the grant at once, with today's codes and texts (`authority_lost`, `grant_revoked`); a narrowing uses `authority_removed`.
6. **Deck lifecycle (P4b-3b).** `owner_start` is unchanged; `station_spec` (derived from the stored grant) is used by the live paths only (`allow_recording` = a non-zero recording cap). A give is refused while the session holds any owner grant, also one without a row (a grant from before P4b). A failed give revokes its grant (`attach_failed`) only while the station never acknowledged a set for the session; a failed later change changes nothing (reconcile repeats the latest state). A second revoke queues nothing. Terminal = revoked, expired for a reason other than idle, or past its lifetime.
7. **Board (P4b-4).** The give and change actions are in the Fleet panel only (not in the session header menu, which is at the module size limit); the board shows them for a station whose link negotiated `coordinator_live.v1` (`StationView.coordinator_live`) and a grant that has a row (`GrantView.live`).
